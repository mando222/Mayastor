use common::MayastorTest;

use mayastor::{
    core::{Bdev, BdevHandle, MayastorCliArgs, Share},
    ffihelper::cb_arg,
    lvs::{Lvol, Lvs},
    nexus_uri::{bdev_create, bdev_destroy},
};

use rpc::mayastor::CreatePoolRequest;

use futures::channel::oneshot;
use std::{
    ffi::{c_void, CString},
    io::{Error, ErrorKind},
    mem::MaybeUninit,
};

use spdk_sys::{spdk_bdev_free_io, spdk_bdev_io, spdk_bdev_unmap};

use nvmeadm::NvmeTarget;
use std::convert::TryFrom;

use tracing::info;

pub mod common;

static BDEVNAME1: &str = "aio:///tmp/disk1.img";
static DISKNAME1: &str = "/tmp/disk1.img";

// Get the I/O blocksize for the filesystem upon which the given file resides.
fn get_fs_blocksize(path: &str) -> Result<u64, Error> {
    Ok(stat(path)?.st_blksize as u64)
}

// Return the number of (512-byte) blocks allocated for a given file.
// Note that the result is ALWAYS in terms of 512-byte blocks,
// regardless of the underlying filesystem I/O blocksize.
fn get_allocated_blocks(path: &str) -> Result<u64, Error> {
    Ok(stat(path)?.st_blocks as u64)
}

// Obtain stat information for given file
fn stat(path: &str) -> Result<libc::stat64, Error> {
    let mut data: MaybeUninit<libc::stat64> = MaybeUninit::uninit();
    let cpath = CString::new(path).unwrap();

    if unsafe { libc::stat64(cpath.as_ptr(), data.as_mut_ptr()) } < 0 {
        return Err(Error::last_os_error());
    }

    Ok(unsafe { data.assume_init() })
}

// Unmap a specified region.
// Partial blocks are zeroed rather than deallocated.
// This means that no blocks will be deallocated if the
// region is smaller than the filesystem I/O blocksize.
async fn unmap(
    handle: &BdevHandle,
    offset: u64,
    nbytes: u64,
) -> Result<(), Error> {
    extern "C" fn io_completion_cb(
        io: *mut spdk_bdev_io,
        success: bool,
        arg: *mut c_void,
    ) {
        let sender = unsafe {
            Box::from_raw(arg as *const _ as *mut oneshot::Sender<bool>)
        };
        unsafe {
            spdk_bdev_free_io(io);
        }
        sender
            .send(success)
            .expect("io completion callback - receiver side disappeared");
    }

    let (sender, receiver) = oneshot::channel::<bool>();

    let errno = unsafe {
        spdk_bdev_unmap(
            handle.desc.as_ptr(),
            handle.channel.as_ptr(),
            offset,
            nbytes,
            Some(io_completion_cb),
            cb_arg(sender),
        )
    };

    if errno != 0 {
        return Err(Error::from_raw_os_error(errno.abs()));
    }

    if receiver.await.expect("failed awaiting unmap completion") {
        return Ok(());
    }

    Err(Error::new(ErrorKind::Other, "unmap failed"))
}

#[tokio::test]
#[ignore = "requires updated spdk"]
async fn unmap_bdev_test() {
    common::delete_file(&[DISKNAME1.into()]);
    common::truncate_file(DISKNAME1, 64 * 1024);

    // Get underlying filesystem I/O blocksize.
    let fs_blocksize = get_fs_blocksize(DISKNAME1).unwrap();
    info!("filesystem blocksize is {}", fs_blocksize);

    // Check that size of the file is sufficiently large to contain at least 16
    // blocks
    assert!(
        16 * fs_blocksize <= 64 * 1024,
        "file {} is too small to contain 16 blocks ({} bytes)",
        DISKNAME1,
        16 * fs_blocksize
    );

    // Verify that there are currently no blocks allocated
    // for the sparse file that is our backing store.
    assert_eq!(
        get_allocated_blocks(DISKNAME1).unwrap(),
        0,
        "expected 0 blocks"
    );

    let args = MayastorCliArgs {
        reactor_mask: "0x3".into(),
        ..Default::default()
    };

    let ms = MayastorTest::new(args);

    // Create bdev but do not perform any I/O.
    ms.spawn(async {
        bdev_create(BDEVNAME1).await.expect("failed to create bdev");
        let handle = BdevHandle::open(BDEVNAME1, true, true)
            .expect("failed to obtain bdev handle");
        handle.close();
        bdev_destroy(BDEVNAME1).await.unwrap();
    })
    .await;

    // Verify that number of allocated blocks is still 0.
    assert_eq!(
        get_allocated_blocks(DISKNAME1).unwrap(),
        0,
        "expected 0 blocks"
    );

    // Create a bdev and write 10 blocks at an offset of 4 blocks.
    let blocksize = fs_blocksize;
    ms.spawn(async move {
        bdev_create(BDEVNAME1).await.expect("failed to create bdev");
        let handle = BdevHandle::open(BDEVNAME1, true, true)
            .expect("failed to obtain bdev handle");

        let mut buf = handle.dma_malloc(10 * blocksize).unwrap();
        buf.fill(0xff);

        info!("writing 10 blocks");
        handle.write_at(4 * blocksize, &buf).await.unwrap();

        handle.close();
        bdev_destroy(BDEVNAME1).await.unwrap();
    })
    .await;

    // Verify that 10 blocks have been allocated.
    assert_eq!(
        get_allocated_blocks(DISKNAME1).unwrap() * 512,
        10 * fs_blocksize,
        "expected 10 blocks ({} bytes)",
        10 * fs_blocksize
    );

    // Create bdev and unmap 4 blocks at an offset of 6 blocks.
    let blocksize = fs_blocksize;
    ms.spawn(async move {
        bdev_create(BDEVNAME1).await.expect("failed to create bdev");
        let handle = BdevHandle::open(BDEVNAME1, true, true)
            .expect("failed to obtain bdev handle");

        info!("unmapping 4 blocks");
        unmap(&handle, 6 * blocksize, 4 * blocksize).await.unwrap();

        handle.close();
        bdev_destroy(BDEVNAME1).await.unwrap();
    })
    .await;

    // Verify that number of allocated blocks has been reduced to 6.
    assert_eq!(
        get_allocated_blocks(DISKNAME1).unwrap() * 512,
        6 * fs_blocksize,
        "expected 6 blocks ({} bytes)",
        6 * fs_blocksize
    );

    // Create bdev and unmap 4 blocks at an offset of 10 blocks.
    let blocksize = fs_blocksize;
    ms.spawn(async move {
        bdev_create(BDEVNAME1).await.expect("failed to create bdev");
        let handle = BdevHandle::open(BDEVNAME1, true, true)
            .expect("failed to obtain bdev handle");

        info!("unmapping 4 blocks");
        unmap(&handle, (6 + 4) * blocksize, 4 * blocksize)
            .await
            .unwrap();

        handle.close();
        bdev_destroy(BDEVNAME1).await.unwrap();
    })
    .await;

    // Verify that number of allocated blocks has been reduced to 2.
    assert_eq!(
        get_allocated_blocks(DISKNAME1).unwrap() * 512,
        2 * fs_blocksize,
        "expected 2 blocks ({} bytes)",
        2 * fs_blocksize
    );

    if fs_blocksize > 1024 {
        // Create bdev and unmap 1024 bytes at an offset of 4 blocks.
        // This is less than the underlying filesystem allocation unit size
        // (fs_blocksize), and is too small for any deallocation to occur.
        // The specified region is zeroed but the number of allocated
        // blocks should not change.
        let blocksize = fs_blocksize;
        ms.spawn(async move {
            bdev_create(BDEVNAME1).await.expect("failed to create bdev");
            let handle = BdevHandle::open(BDEVNAME1, true, true)
                .expect("failed to obtain bdev handle");

            info!("unmapping 1024 BYTES");
            unmap(&handle, 4 * blocksize, 1024).await.unwrap();

            handle.close();
            bdev_destroy(BDEVNAME1).await.unwrap();
        })
        .await;

        // Verify that number of allocated blocks has not changed.
        assert_eq!(
            get_allocated_blocks(DISKNAME1).unwrap() * 512,
            2 * fs_blocksize,
            "expected 2 blocks ({} bytes)",
            2 * fs_blocksize
        );
    }

    // Create bdev and unmap 2 blocks at an offset of 4 blocks.
    let blocksize = fs_blocksize;
    ms.spawn(async move {
        bdev_create(BDEVNAME1).await.expect("failed to create bdev");
        let handle = BdevHandle::open(BDEVNAME1, true, true)
            .expect("failed to obtain bdev handle");

        info!("unmapping 2 blocks");
        unmap(&handle, 4 * blocksize, 2 * blocksize).await.unwrap();

        handle.close();
        bdev_destroy(BDEVNAME1).await.unwrap();
    })
    .await;

    // Verify that number of allocated blocks has been reduced to 0.
    assert_eq!(
        get_allocated_blocks(DISKNAME1).unwrap(),
        0,
        "expected 0 blocks"
    );

    common::delete_file(&[DISKNAME1.into()]);
}

#[tokio::test]
async fn unmap_lvol_test() {
    const NUM_VOLS: usize = 4;
    const FILE_SIZE: u64 = 64 * 1024;
    const VOL_SIZE: u64 = FILE_SIZE / (NUM_VOLS as u64);

    common::delete_file(&[DISKNAME1.into()]);
    common::truncate_file(DISKNAME1, FILE_SIZE);

    let args = MayastorCliArgs {
        reactor_mask: "0x3".into(),
        ..Default::default()
    };
    let ms = MayastorTest::new(args);

    // Create a pool.
    ms.spawn(async {
        Lvs::create_or_import(CreatePoolRequest {
            name: "tpool".into(),
            disks: vec![BDEVNAME1.into()],
        })
        .await
        .unwrap();
    })
    .await;

    // Check that we're able to find our new LVS.
    ms.spawn(async {
        assert_eq!(Lvs::iter().count(), 1);
        let pool = Lvs::lookup("tpool").unwrap();
        info!("created pool: name={} UUID={}", pool.name(), pool.uuid());
        assert_eq!(pool.name(), "tpool");
        assert_eq!(pool.used(), 0);
        assert_eq!(pool.base_bdev().name(), DISKNAME1);
    })
    .await;

    // Create lvols on this pool.
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        for i in 0 .. NUM_VOLS {
            pool.create_lvol(&format!("vol-{}", i), VOL_SIZE, true)
                .await
                .unwrap();
        }
    })
    .await;

    // Share all replicas.
    let targets = ms
        .spawn(async {
            let pool = Lvs::lookup("tpool").unwrap();
            assert_eq!(pool.lvols().unwrap().count(), NUM_VOLS);

            let mut targets: Vec<NvmeTarget> = Vec::new();

            for vol in pool.lvols().unwrap() {
                vol.share_nvmf().await.unwrap();
                let uri = vol.share_uri().unwrap();
                info!("lvol {} shared as: {}", vol.name(), uri);
                targets.push(NvmeTarget::try_from(uri).unwrap());
            }

            targets
        })
        .await;

    let mut devlist: Vec<String> = Vec::new();

    // Attach all targets.
    for target in &targets {
        let devices = target.connect().unwrap();
        let dev = devices[0].path.to_string();
        info!("nvmf target attached to device: {}", dev);
        devlist.push(dev);
    }

    // Write to all devices
    for dev in &devlist {
        info!("writing to {} with dd ...", dev);
        common::dd_urandom_blkdev(&dev);
    }

    // Disconnect all targets.
    for target in &targets {
        info!("disconnecting target");
        target.disconnect().unwrap();
    }

    assert!(
        get_allocated_blocks(DISKNAME1).unwrap() > 0,
        "number of allocated blocks should be non-zero"
    );

    // Destroy the lvols.
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();

        let vols: Vec<Lvol> = pool.lvols().unwrap().collect();
        assert_eq!(vols.len(), NUM_VOLS);

        for vol in vols {
            vol.destroy().await.unwrap();
        }
    })
    .await;

    // Destroy the pool
    ms.spawn(async {
        let pool = Lvs::lookup("tpool").unwrap();
        assert_eq!(pool.lvols().unwrap().count(), 0);

        pool.destroy().await.unwrap();
    })
    .await;

    // Validate the expected state of mayastor.
    ms.spawn(async {
        // pools destroyed
        assert_eq!(Lvs::iter().count(), 0);

        // no bdevs
        assert_eq!(Bdev::bdev_first().into_iter().count(), 0);
    })
    .await;

    info!(
        "{} blocks allocated for {}",
        get_allocated_blocks(DISKNAME1).unwrap(),
        DISKNAME1
    );

    common::delete_file(&[DISKNAME1.into()]);
}
