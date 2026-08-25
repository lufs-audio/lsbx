//! Builds the libvirt domain XML for `create_from_golden`.
//!
//! Memory is expressed as `KiB` in the domain XML (libvirt's native unit
//! for `<memory>`/`<currentMemory>`); this module parses the
//! `CreateFromGoldenRequest::memory` string (`"512M"`, `"2G"`, `"1024"` —
//! matching the existing manifest's own `memory: String` convention, per
//! SPEC.md §4.1/Unit 08's `GoldenConfig.memory: String`) into KiB itself,
//! since neither the kernel type nor this crate's own request type parses
//! it for us.

use lsbx_kernel::error::LsbxError;

/// Parses a memory size string (`"512M"`, `"2G"`, `"1024"` — bare digits
/// assumed to already be MiB, matching the existing Python system's
/// convention for an un-suffixed `memory` value) into KiB.
pub fn parse_memory_to_kib(memory: &str) -> Result<u64, LsbxError> {
    let trimmed = memory.trim();
    if trimmed.is_empty() {
        return Err(LsbxError::Usage("memory value is empty".to_string()));
    }

    let (digits, multiplier_kib): (&str, u64) =
        if let Some(stripped) = trimmed.strip_suffix("GiB").or_else(|| trimmed.strip_suffix("gib")) {
            (stripped, 1024 * 1024)
        } else if let Some(stripped) = trimmed.strip_suffix("GB").or_else(|| trimmed.strip_suffix("gb")) {
            (stripped, 1024 * 1024)
        } else if let Some(stripped) = trimmed.strip_suffix(['G', 'g']) {
            (stripped, 1024 * 1024)
        } else if let Some(stripped) = trimmed.strip_suffix("MiB").or_else(|| trimmed.strip_suffix("mib")) {
            (stripped, 1024)
        } else if let Some(stripped) = trimmed.strip_suffix("MB").or_else(|| trimmed.strip_suffix("mb")) {
            (stripped, 1024)
        } else if let Some(stripped) = trimmed.strip_suffix(['M', 'm']) {
            (stripped, 1024)
        } else if let Some(stripped) = trimmed.strip_suffix(['K', 'k']) {
            (stripped, 1)
        } else {
            (trimmed, 1024) // bare number: assume MiB, same as the existing system.
        };

    let value: u64 = digits
        .trim()
        .parse()
        .map_err(|_| LsbxError::Usage(format!("invalid memory value: '{memory}'")))?;

    Ok(value * multiplier_kib)
}

/// Escapes the handful of characters that are structurally unsafe inside
/// XML text/attribute content. Every value this module interpolates
/// (`name`, `pubkey`, a filesystem path) is untrusted relative to XML
/// syntax — a golden name or pubkey comment containing `<`/`&`/`"` must
/// never be able to inject or break out of an XML node.
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Parameters needed to render a domain XML, gathered from
/// `CreateFromGoldenRequest` plus the disk path this crate resolved
/// separately (see `crate::golden_disk`).
pub struct DomainXmlParams<'a> {
    pub name: &'a str,
    pub cpu: u32,
    pub memory: &'a str,
    pub disk_path: &'a std::path::Path,
    /// Optional cloud-init seed ISO path. When `Some`, an IDE cdrom device
    /// is added to the domain XML so the guest can read cloud-init
    /// `user-data`/`meta-data` at boot (SSH key injection, hostname, etc.).
    pub seed_iso: Option<&'a std::path::Path>,
}

/// Renders a KVM/QEMU domain XML matching the Python reference
/// implementation (`lufs_sandbox/backends/libvirt.py:_domain_xml`):
///
/// - qcow2 disk (virtio)
/// - optional cloud-init seed ISO (IDE cdrom) for SSH key injection
/// - virtio NIC on the `default` libvirt network
/// - serial/pty + console/pty (matching Python's `-serial mon:stdio`)
/// - QEMU guest agent channel (`org.qemu.guest_agent.0`) — required for
///   `virsh domifaddr --source agent` IP resolution
/// - VNC graphics (autoport) for noVNC/WebSocket proxy access
pub fn render_domain_xml(params: &DomainXmlParams<'_>, pubkey: &str) -> Result<String, LsbxError> {
    let memory_kib = parse_memory_to_kib(params.memory)?;
    let name = xml_escape(params.name);
    let disk_path = xml_escape(&params.disk_path.to_string_lossy());
    let pubkey_escaped = xml_escape(pubkey);

    // Cloud-init seed ISO cdrom device (IDE bus, matching Python)
    let seed_disk = match params.seed_iso {
        Some(path) => {
            let seed_path = xml_escape(&path.to_string_lossy());
            format!(
                r#"    <disk type='file' device='cdrom'>
      <source file='{seed_path}'/>
      <target dev='hda' bus='ide'/>
      <readonly/>
    </disk>"#
            )
        }
        None => String::new(),
    };

    Ok(format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='KiB'>{memory_kib}</memory>
  <currentMemory unit='KiB'>{memory_kib}</currentMemory>
  <vcpu placement='static'>{cpu}</vcpu>
  <os>
    <type arch='x86_64' machine='pc'>hvm</type>
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough'/>
  <metadata>
    <lsbx:pubkey xmlns:lsbx="https://lufs.org/lsbx/domain-metadata">{pubkey_escaped}</lsbx:pubkey>
  </metadata>
  <devices>
    <emulator>/usr/bin/qemu-system-x86_64</emulator>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='{disk_path}'/>
      <target dev='vda' bus='virtio'/>
    </disk>
{seed_disk}    <interface type='network'>
      <source network='default'/>
      <model type='virtio'/>
    </interface>
    <serial type='pty'/>
    <console type='pty'/>
    <channel type='unix'>
      <source mode='bind'/>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <graphics type='vnc' port='-1' autoport='yes'/>
  </devices>
</domain>"#,
        name = name,
        memory_kib = memory_kib,
        cpu = params.cpu,
        pubkey_escaped = pubkey_escaped,
        disk_path = disk_path,
        seed_disk = seed_disk,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_gigabyte_suffix() {
        assert_eq!(parse_memory_to_kib("2G").unwrap(), 2 * 1024 * 1024);
    }

    #[test]
    fn parses_gb_suffix() {
        assert_eq!(parse_memory_to_kib("4GB").unwrap(), 4 * 1024 * 1024);
    }

    #[test]
    fn parses_gib_suffix() {
        assert_eq!(parse_memory_to_kib("2GiB").unwrap(), 2 * 1024 * 1024);
    }

    #[test]
    fn parses_megabyte_suffix() {
        assert_eq!(parse_memory_to_kib("512M").unwrap(), 512 * 1024);
    }

    #[test]
    fn parses_mb_suffix() {
        assert_eq!(parse_memory_to_kib("512MB").unwrap(), 512 * 1024);
    }

    #[test]
    fn parses_mib_suffix() {
        assert_eq!(parse_memory_to_kib("512MiB").unwrap(), 512 * 1024);
    }

    #[test]
    fn parses_kilobyte_suffix() {
        assert_eq!(parse_memory_to_kib("2048K").unwrap(), 2048);
    }

    #[test]
    fn bare_number_assumed_mib() {
        assert_eq!(parse_memory_to_kib("1024").unwrap(), 1024 * 1024);
    }

    #[test]
    fn rejects_empty_memory() {
        assert!(parse_memory_to_kib("").is_err());
    }

    #[test]
    fn rejects_non_numeric_memory() {
        assert!(parse_memory_to_kib("lots").is_err());
    }

    #[test]
    fn domain_xml_contains_resolved_disk_path_and_cpu_count() {
        let params = DomainXmlParams {
            name: "lsbx-test-vm",
            cpu: 4,
            memory: "1G",
            disk_path: std::path::Path::new("/var/lib/lsbx/vms/lsbx-test-vm.qcow2"),
            seed_iso: None,
        };
        let xml = render_domain_xml(&params, "ssh-ed25519 AAAA... lsbx:test").unwrap();
        assert!(xml.contains("<name>lsbx-test-vm</name>"));
        assert!(xml.contains("vcpu placement='static'>4<"));
        assert!(xml.contains("/var/lib/lsbx/vms/lsbx-test-vm.qcow2"));
        assert!(xml.contains("1048576")); // 1G in KiB
        assert!(xml.contains("org.qemu.guest_agent.0"));
        assert!(xml.contains("<serial type='pty'/>"));
        assert!(xml.contains("<console type='pty'/>"));
        assert!(!xml.contains("<target dev='hda'")); // no cdrom when no seed
    }

    #[test]
    fn domain_xml_escapes_unsafe_characters_in_name_and_pubkey() {
        let params = DomainXmlParams {
            name: "lsbx-<injected>",
            cpu: 1,
            memory: "512M",
            disk_path: std::path::Path::new("/tmp/x.qcow2"),
            seed_iso: None,
        };
        let xml = render_domain_xml(&params, "ssh-ed25519 AAAA\"quote lsbx:test").unwrap();
        assert!(!xml.contains("<name>lsbx-<injected></name>"));
        assert!(xml.contains("&lt;injected&gt;"));
        assert!(xml.contains("&quot;quote"));
    }

    #[test]
    fn domain_xml_includes_seed_iso_cdrom_when_provided() {
        let params = DomainXmlParams {
            name: "lsbx-test-vm",
            cpu: 2,
            memory: "1G",
            disk_path: std::path::Path::new("/var/lib/lsbx/vms/lsbx-test-vm.qcow2"),
            seed_iso: Some(std::path::Path::new("/var/lib/lsbx/vms/lsbx-test-vm-cidata.iso")),
        };
        let xml = render_domain_xml(&params, "ssh-ed25519 AAAA... test").unwrap();
        assert!(xml.contains("<target dev='hda' bus='ide'/>"));
        assert!(xml.contains("/var/lib/lsbx/vms/lsbx-test-vm-cidata.iso"));
        assert!(xml.contains("device='cdrom'"));
    }

    #[test]
    fn domain_xml_propagates_invalid_memory_error() {
        let params = DomainXmlParams {
            name: "lsbx-test-vm",
            cpu: 1,
            memory: "not-a-number",
            disk_path: std::path::Path::new("/tmp/x.qcow2"),
            seed_iso: None,
        };
        assert!(render_domain_xml(&params, "irrelevant").is_err());
    }
}
