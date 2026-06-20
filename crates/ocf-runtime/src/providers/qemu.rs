//! QEMU/KVM virtual-machine backend, driven through libvirt's `virsh`.
//!
//! Unlike the container backends, QEMU is migration-capable: it overrides
//! `dump_memory`/`restore` and reports `supports_migration() == true`, so it can
//! be wired into a [`crate::migration::Migrator`].
//!
//! Every operation shells out to `virsh` (libvirt). Domain state is owned by
//! libvirt and read back via `virsh domstate` / `virsh list`; this backend keeps
//! no shadow copy.
//!
//! Migration uses the `virsh save` / `virsh restore` pair (rather than
//! `virsh dump`, which produces a non-restorable core dump): `save` writes a
//! restorable memory image and suspends the source domain — exactly the
//! "checkpoint + quiesce source" semantics a live migration needs — and
//! `restore` boots a domain from that image on the target.

use crate::provider::RuntimeProvider;
use crate::providers::command::{self, parse_virsh_state, workloads_from_virsh_list};
use crate::workload::{MemorySnapshot, RuntimeKind, Workload};
use ocf_core::prelude::*;
use std::path::PathBuf;

/// The `virsh` binary this backend shells out to.
const BIN: &str = "virsh";

/// A [`RuntimeProvider`] backed by QEMU/KVM via libvirt.
#[derive(Default)]
pub struct QemuRuntime;

impl QemuRuntime {
    pub fn new() -> Self {
        Self
    }

    /// Path of the memory-image file for a workload's checkpoint, under the
    /// system temp directory. Source and target agree on this path so the
    /// migrator can ship the blob to the same location on the target host.
    fn save_file(id: &Id) -> PathBuf {
        std::env::temp_dir().join(format!("ocf-vm-{id}.save"))
    }

    /// Build the libvirt domain XML for a workload.
    ///
    /// A minimal, valid `<domain type='kvm'>` with the requested memory and
    /// vCPUs and the workload image as a qcow2 disk. Memory is rounded down to
    /// whole KiB (libvirt's unit) with a 64 MiB floor so a resource-less
    /// workload still defines.
    fn domain_xml(workload: &Workload) -> String {
        let id = workload.metadata.id.to_string();
        let mem_kib = (workload.resources.memory_bytes / 1024).max(64 * 1024);
        // At least one vCPU; millicores rounded up to whole cores.
        let vcpus = ((workload.resources.cpu_millis + 999) / 1000).max(1);
        let image = xml_escape(&workload.image);
        let name = xml_escape(&id);
        format!(
            "<domain type='kvm'>\
               <name>{name}</name>\
               <memory unit='KiB'>{mem_kib}</memory>\
               <vcpu>{vcpus}</vcpu>\
               <os><type arch='x86_64'>hvm</type></os>\
               <devices>\
                 <disk type='file' device='disk'>\
                   <driver name='qemu' type='qcow2'/>\
                   <source file='{image}'/>\
                   <target dev='vda' bus='virtio'/>\
                 </disk>\
               </devices>\
             </domain>"
        )
    }
}

/// Minimal XML attribute/text escaping for the few fields we interpolate.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

impl Provider for QemuRuntime {
    fn name(&self) -> &str {
        "qemu"
    }
    fn description(&self) -> &str {
        "QEMU/KVM virtual-machine backend (migration-capable)"
    }
}

#[async_trait]
impl RuntimeProvider for QemuRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::VirtualMachine
    }

    fn supports_migration(&self) -> bool {
        true
    }

    async fn create(&self, workload: &Workload) -> Result<()> {
        command::require_kind(workload, RuntimeKind::VirtualMachine, self.name())?;
        // libvirt defines a domain from an XML file: write the generated XML to
        // a temp file, then `virsh define <file>`.
        let xml = Self::domain_xml(workload);
        let xml_path =
            std::env::temp_dir().join(format!("ocf-vm-{}.xml", workload.metadata.id));
        std::fs::write(&xml_path, xml)
            .map_err(|e| Error::provider(BIN, format!("writing domain XML: {e}")))?;

        let args = vec!["define".to_string(), path_arg(&xml_path)?];
        let result = command::run(BIN, &args).await.map(|_| ());
        // The definition is now owned by libvirt; the scratch XML is no longer
        // needed regardless of outcome.
        let _ = std::fs::remove_file(&xml_path);
        result
    }

    async fn start(&self, id: &Id) -> Result<()> {
        // `virsh start <id>`
        let args = vec!["start".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn stop(&self, id: &Id) -> Result<()> {
        // `virsh shutdown <id>` (graceful ACPI powerdown).
        let args = vec!["shutdown".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        // Force off if still running (best-effort), then remove the definition.
        let destroy = vec!["destroy".to_string(), id.to_string()];
        let _ = command::run(BIN, &destroy).await;
        // `virsh undefine <id>`
        let args = vec!["undefine".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn status(&self, id: &Id) -> Result<LifecycleState> {
        // `virsh domstate <id>`
        let args = vec!["domstate".to_string(), id.to_string()];
        let out = command::run(BIN, &args).await?;
        Ok(parse_virsh_state(&out))
    }

    async fn list(&self) -> Result<Vec<Workload>> {
        // `virsh list --all --name` — one domain name per line.
        let args = vec![
            "list".to_string(),
            "--all".to_string(),
            "--name".to_string(),
        ];
        let out = command::run(BIN, &args).await?;
        Ok(workloads_from_virsh_list(&out))
    }

    async fn dump_memory(&self, id: &Id) -> Result<MemorySnapshot> {
        // `virsh save <dom> <file>`: write a restorable memory image and suspend
        // the source domain. The blob_ref is the on-disk image path so `restore`
        // (here or on the migration target) can consume it directly.
        let file = Self::save_file(id);
        let path = path_arg(&file)?;
        let args = vec!["save".to_string(), id.to_string(), path.clone()];
        command::run(BIN, &args).await?;

        // The real captured size is the size of the save image libvirt produced.
        let bytes_len = std::fs::metadata(&file)
            .map_err(|e| Error::provider(BIN, format!("stat save image: {e}")))?
            .len();

        Ok(MemorySnapshot {
            workload_id: id.clone(),
            node: None,
            bytes_len,
            blob_ref: path,
        })
    }

    async fn restore(&self, snapshot: &MemorySnapshot) -> Result<()> {
        // `virsh restore <file>`: boot a domain from the saved memory image.
        let args = vec!["restore".to_string(), snapshot.blob_ref.clone()];
        command::run(BIN, &args).await.map(|_| ())
    }
}

/// Render a filesystem path as a UTF-8 command argument, rejecting paths that
/// are not valid UTF-8 (which `virsh` could not receive cleanly anyway).
fn path_arg(path: &std::path::Path) -> Result<String> {
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| Error::provider(BIN, format!("non-UTF-8 path {path:?}")))
}
