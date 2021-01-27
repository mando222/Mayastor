const _ = require('lodash');
const assert = require('chai').assert;
const path = require('path');
const protoLoader = require('@grpc/proto-loader');
const grpc = require('grpc-uds');
const enums = require('./grpc_enums');

// each stat is incremented by this each time when stat method is called
const STAT_DELTA = 1000;

// The problem is that the grpc server creates the keys from proto file
// even if they don't exist. So we have to test that the key is there
// but also that it has not a default value (empty string, zero, ...).
function assertHasKeys (obj, keys, empty) {
  empty = empty || [];
  for (const key in obj) {
    if (keys.indexOf(key) < 0) {
      assert(
        false,
        'Extra parameter "' + key + '" in object ' + JSON.stringify(obj)
      );
    }
  }
  for (let i = 0; i < keys.length; i++) {
    const key = keys[i];
    const val = obj[key];
    if (
      val == null ||
      // no way to check boolean
      (typeof val === 'string' && val.length === 0 && empty.indexOf(key) < 0) ||
      (typeof val === 'number' && val === 0 && empty.indexOf(key) < 0)
    ) {
      assert(
        false,
        'Missing property ' + key + ' in object ' + JSON.stringify(obj)
      );
    }
  }
}

// Create mayastor mock grpc server with preconfigured storage pool, replica
// and nexus objects. Pools can be added & deleted by means of grpc calls.
// The actual state (i.e. list of pools) can be retrieved by get*() method.
class MayastorServer {
  constructor (endpoint, pools, replicas, nexus) {
    const packageDefinition = protoLoader.loadSync(
      path.join(__dirname, '..', 'proto', 'mayastor.proto'),
      {
        keepCase: false,
        longs: Number,
        enums: String,
        defaults: true,
        oneofs: true
      }
    );
    const mayastor = grpc.loadPackageDefinition(packageDefinition).mayastor;
    const srv = new grpc.Server();

    this.pools = _.cloneDeep(pools || []);
    this.replicas = _.cloneDeep(replicas || []);
    this.nexus = _.cloneDeep(nexus || []);
    this.statCounter = 0;

    const self = this;
    srv.addService(mayastor.Mayastor.service, {
      // When a pool is created we implicitly set state to POOL_ONLINE,
      // capacity to 100 and used to 4.
      createPool: (call, cb) => {
        const args = call.request;
        assertHasKeys(
          args,
          ['name', 'disks'],
          []
        );
        let pool = self.pools.find((p) => p.name === args.name);
        if (!pool) {
          pool = {
            name: args.name,
            disks: args.disks.map((d) => `aio://${d}`),
            state: enums.POOL_ONLINE,
            capacity: 100,
            used: 4
          };
          self.pools.push(pool);
        }
        cb(null, pool);
      },
      destroyPool: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['name']);
        const idx = self.pools.findIndex((p) => p.name === args.name);
        if (idx >= 0) {
          self.pools.splice(idx, 1);
        }
        cb(null, {});
      },
      listPools: (_unused, cb) => {
        cb(null, {
          pools: self.pools
        });
      },
      createReplica: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid', 'pool', 'size', 'thin', 'share']);
        let r = self.replicas.find((r) => r.uuid === args.uuid);
        if (r) {
          return cb(null, r);
        }
        const pool = self.pools.find((p) => p.name === args.pool);
        if (!pool) {
          const err = new Error('pool not found');
          err.code = grpc.status.NOT_FOUND;
          return cb(err);
        }
        if (!args.thin) {
          pool.used += args.size;
        }
        let uri;
        if (args.share === 'REPLICA_NONE') {
          uri = 'bdev:///' + args.uuid;
        } else if (args.share === 'REPLICA_ISCSI') {
          uri = 'iscsi://192.168.0.1:3800/' + args.uuid;
        } else {
          uri = 'nvmf://192.168.0.1:4020/' + args.uuid;
        }

        r = {
          uuid: args.uuid,
          pool: args.pool,
          size: args.size,
          thin: args.thin,
          share: args.share,
          uri
        };
        self.replicas.push(r);
        cb(null, r);
      },
      destroyReplica: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid']);
        const idx = self.replicas.findIndex((r) => r.uuid === args.uuid);
        if (idx >= 0) {
          const r = self.replicas.splice(idx, 1)[0];
          if (!r.thin) {
            const pool = self.pools.find((p) => p.name === r.pool);
            pool.used -= r.size;
          }
        }
        cb(null, {});
      },
      listReplicas: (_unused, cb) => {
        cb(null, { replicas: self.replicas });
      },
      statReplicas: (_unused, cb) => {
        self.statCounter += STAT_DELTA;
        cb(null, {
          replicas: self.replicas.map((r) => {
            return {
              uuid: r.uuid,
              pool: r.pool,
              stats: {
                numReadOps: self.statCounter,
                numWriteOps: self.statCounter,
                bytesRead: self.statCounter,
                bytesWritten: self.statCounter
              }
            };
          })
        });
      },
      shareReplica: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid', 'share']);
        const r = self.replicas.find((ent) => ent.uuid === args.uuid);
        if (!r) {
          const err = new Error('not found');
          err.code = grpc.status.NOT_FOUND;
          return cb(err);
        }
        if (args.share === 'REPLICA_NONE') {
          r.uri = 'bdev:///' + r.uuid;
        } else if (args.share === 'REPLICA_ISCSI') {
          r.uri = 'iscsi://192.168.0.1:3800/' + r.uuid;
        } else if (args.share === 'REPLICA_NVMF') {
          r.uri = 'nvmf://192.168.0.1:4020/' + r.uuid;
        } else {
          assert(false, 'Invalid share protocol');
        }
        r.share = args.share;
        cb(null, {
          uri: r.uri
        });
      },
      createNexus: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid', 'size', 'children']);
        let nexus = self.nexus.find((r) => r.uuid === args.uuid);
        if (!nexus) {
          nexus = {
            uuid: args.uuid,
            size: args.size,
            state: enums.NEXUS_ONLINE,
            children: args.children.map((r) => {
              return {
                uri: r,
                state: enums.CHILD_ONLINE,
                rebuildProgress: 0
              };
            })
            // device_path omitted
          };
          self.nexus.push(nexus);
        }
        cb(null, nexus);
      },
      destroyNexus: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid']);
        const idx = self.nexus.findIndex((n) => n.uuid === args.uuid);
        if (idx >= 0) {
          self.nexus.splice(idx, 1);
        }
        cb(null, {});
      },
      listNexus: (_unused, cb) => {
        cb(null, { nexusList: self.nexus });
      },
      publishNexus: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid', 'share', 'key'], ['key']);
        assert.equal(1, args.share); // Must be value of NEXUS_NVMF for now
        const idx = self.nexus.findIndex((n) => n.uuid === args.uuid);
        if (idx >= 0) {
          self.nexus[idx].deviceUri = 'nvmf://host/nqn';
          cb(null, {
            deviceUri: 'nvmf://host/nqn'
          });
        } else {
          const err = new Error('not found');
          err.code = grpc.status.NOT_FOUND;
          cb(err);
        }
      },
      unpublishNexus: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid']);
        const idx = self.nexus.findIndex((n) => n.uuid === args.uuid);
        if (idx >= 0) {
          delete self.nexus[idx].deviceUri;
          cb(null, {});
        } else {
          const err = new Error('not found');
          err.code = grpc.status.NOT_FOUND;
          cb(err);
        }
      },
      addChildNexus: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid', 'uri', 'norebuild']);
        const n = self.nexus.find((n) => n.uuid === args.uuid);
        if (!n) {
          const err = new Error('not found');
          err.code = grpc.status.NOT_FOUND;
          return cb(err);
        }
        if (!n.children.find((ch) => ch.uri === args.uri)) {
          n.children.push({
            uri: args.uri,
            state: enums.CHILD_DEGRADED
          });
        }
        cb(null, {
          uri: args.uri,
          state: enums.CHILD_DEGRADED,
          rebuildProgress: 0
        });
      },
      removeChildNexus: (call, cb) => {
        const args = call.request;
        assertHasKeys(args, ['uuid', 'uri']);
        const n = self.nexus.find((n) => n.uuid === args.uuid);
        if (!n) {
          const err = new Error('not found');
          err.code = grpc.status.NOT_FOUND;
          return cb(err);
        }
        n.children = n.children.filter((ch) => ch.uri !== args.uri);
        cb();
      },
      // dummy impl to silence the warning about unimplemented method
      childOperation: (_unused, cb) => {
        cb();
      }
    });
    srv.bind(endpoint, grpc.ServerCredentials.createInsecure());
    this.srv = srv;
  }

  getPools () {
    return this.pools;
  }

  getReplicas () {
    return this.replicas;
  }

  getNexus () {
    return this.nexus;
  }

  start () {
    this.srv.start();
    return this;
  }

  stop () {
    this.srv.forceShutdown();
  }
}

module.exports = {
  MayastorServer,
  STAT_DELTA
};
