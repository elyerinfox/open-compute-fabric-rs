//! The base contract every managed resource implements.

use crate::metadata::Metadata;

/// The "abstract base class" of the domain model.
///
/// Every concrete resource (a workload, a VPC, a disk, a load balancer, ...)
/// implements `Resource`, which guarantees it carries [`Metadata`] and reports
/// a stable `kind` discriminator. Generic machinery (the API serializer, audit
/// logging, the topology indexer) can therefore treat any resource uniformly.
pub trait Resource: Send + Sync {
    /// A stable, lowercase discriminator, e.g. `"workload"`, `"vpc"`, `"disk"`.
    fn kind(&self) -> &'static str;

    /// The resource's metadata block.
    fn metadata(&self) -> &Metadata;

    /// Convenience accessor for the resource's display name.
    fn name(&self) -> &str {
        &self.metadata().name
    }
}
