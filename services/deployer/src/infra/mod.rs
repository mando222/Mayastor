pub mod dns;
pub mod jaeger;
pub mod mayastor;
pub mod nats;
pub mod rest;

pub use ::nats::*;
pub use dns::*;
pub use jaeger::*;
pub use mayastor::*;
pub use rest::*;

use super::StartOptions;
use async_trait::async_trait;
use composer::{Binary, Builder, BuilderConfigure, ComposeTest, ContainerSpec};
use mbus_api::{
    v0::{ChannelVs, Liveness},
    Message,
};
use std::{cmp::Ordering, str::FromStr};
use structopt::StructOpt;
use strum::VariantNames;
use strum_macros::{EnumVariantNames, ToString};
pub(crate) type Error = Box<dyn std::error::Error>;

#[macro_export]
macro_rules! impl_ctrlp_services {
    ($($name:ident,)+) => {
        #[derive(Debug, Clone)]
        pub(crate) struct ControlPlaneServices(Vec<ControlPlaneService>);

        #[derive(Debug, Clone, StructOpt, ToString, EnumVariantNames)]
        #[structopt(about = "Control Plane Services")]
        pub(crate) enum ControlPlaneService {
            $(
                $name($name),
            )+
        }

        impl From<&ControlPlaneService> for Component {
            fn from(ctrlp_svc: &ControlPlaneService) -> Self {
                match ctrlp_svc {
                    $(ControlPlaneService::$name(obj) => Component::$name(obj.clone()),)+
                }
            }
        }

        impl FromStr for ControlPlaneService {
            type Err = String;

            fn from_str(source: &str) -> Result<Self, Self::Err> {
                // todo: use mayastor-macros to have a stringify lowercase
                // or iterate all types?
                Ok(match source.trim() {
                    $(stringify!($name) => Self::$name($name::default()),)+
                    _ => return Err(format!(
                        "{} is an invalid type of service! Available types: {:?}",
                        source,
                        Self::VARIANTS
                    )),
                })
            }
        }

        $(#[async_trait]
        impl ComponentAction for $name {
            fn configure(&self, options: &StartOptions, cfg: Builder) -> Result<Builder, Error> {
                let name = stringify!($name).to_ascii_lowercase();
                if options.build {
                    let status = std::process::Command::new("cargo")
                        .args(&["build", "-p", "services", "--bin", &name])
                        .status()?;
                    build_error(&format!("the {} service", name), status.code())?;
                }
                Ok(cfg.add_container_bin(
                    &name,
                    Binary::from_dbg(&name).with_nats("-n"),
                ))
            }
            async fn start(&self, _options: &StartOptions, cfg: &ComposeTest) -> Result<(), Error> {
                let name = stringify!($name).to_ascii_lowercase();
                cfg.start(&name).await?;
                Liveness {}.request_on(ChannelVs::$name).await?;
                Ok(())
            }
        })+
    };
    ($($name:ident), +) => {
        impl_ctrlp_services!($($name,)+);
    };
}

pub(crate) fn build_error(
    name: &str,
    status: Option<i32>,
) -> Result<(), Error> {
    let make_error = |extra: &str| {
        let error = format!("Failed to build {}: {}", name, extra);
        std::io::Error::new(std::io::ErrorKind::Other, error)
    };
    match status {
        Some(0) => Ok(()),
        Some(code) => {
            let error = format!("exited with code {}", code);
            Err(make_error(&error).into())
        }
        None => Err(make_error("interrupted by signal").into()),
    }
}

#[macro_export]
macro_rules! impl_component {
    ($($name:ident,$order:literal,)+) => {
        #[derive(Debug, Clone, StructOpt, ToString, EnumVariantNames, Eq, PartialEq)]
        #[structopt(about = "Control Plane Components")]
        pub(crate) enum Component {
            $(
                $name($name),
            )+
        }

        #[derive(Debug, Clone)]
        pub(crate) struct Components(Vec<Component>, StartOptions);
        impl BuilderConfigure for Components {
            fn configure(&self, cfg: Builder) -> Result<Builder, Error> {
                let mut cfg = cfg;
                for component in &self.0 {
                    cfg = component.configure(&self.1, cfg)?;
                }
                Ok(cfg)
            }
        }

        impl Components {
            pub(crate) fn push_except_service(&mut self, name: &str, component: Component) {
                if !ControlPlaneService::VARIANTS.iter().any(|&s| s == name) {
                    self.0.push(component);
                }
            }
            pub(crate) fn new(options: StartOptions) -> Components {
                let services = options.services.clone();
                let components = services
                    .iter()
                    .map(Component::from)
                    .collect::<Vec<Component>>();

                let mut components = Components(components, options.clone());
                $(components.push_except_service(stringify!($name), $name::default().into());)+
                components.0.sort();
                components
            }
            pub(crate) async fn start(&self, cfg: &ComposeTest) -> Result<(), Error> {
                for component in &self.0 {
                    component.start(&self.1, cfg).await?;
                }
                Ok(())
            }
        }

        #[async_trait]
        pub(crate) trait ComponentAction {
            fn configure(&self, options: &StartOptions, cfg: Builder) -> Result<Builder, Error>;
            async fn start(&self, options: &StartOptions, cfg: &ComposeTest) -> Result<(), Error>;
        }

        #[async_trait]
        impl ComponentAction for Component {
            fn configure(&self, options: &StartOptions, cfg: Builder) -> Result<Builder, Error> {
                match self {
                    $(Self::$name(obj) => obj.configure(options, cfg),)+
                }
            }
            async fn start(&self, options: &StartOptions, cfg: &ComposeTest) -> Result<(), Error> {
                match self {
                    $(Self::$name(obj) => obj.start(options, cfg).await,)+
                }
            }
        }

        $(impl From<$name> for Component {
            fn from(from: $name) -> Component {
                Component::$name(from)
            }
        })+

        $(#[derive(Default, Debug, Clone, StructOpt, Eq, PartialEq)]
        pub(crate) struct $name {})+

        impl Component {
            fn boot_order(&self) -> u32 {
                match self {
                    $(Self::$name(_) => $order,)+
                }
            }
        }

        impl PartialOrd for Component {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                self.boot_order().partial_cmp(&other.boot_order())
            }
        }
        impl Ord for Component {
            fn cmp(&self, other: &Self) -> Ordering {
                self.boot_order().cmp(&other.boot_order())
            }
        }
    };
    ($($name:ident, $order:ident), +) => {
        impl_component!($($name,$order)+);
    };
}

// Component Name and bootstrap ordering
// from lower to high
impl_component! {
    Dns,        0,
    Nats,       0,
    Mayastor,   1,
    Node,       2,
    Pool,       3,
    Volume,     3,
    Rest,       4,
    Jaeger,     4,
}

// Message Bus Control Plane Services
impl_ctrlp_services!(Node, Pool, Volume);
