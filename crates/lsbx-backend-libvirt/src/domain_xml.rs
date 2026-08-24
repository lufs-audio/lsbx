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
        if let Some(stripped) = trimmed.strip_suffix(['G', 'g']) {
            (stripped, 1024 * 1024)
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
}

/// Renders a minimal but complete KVM/QEMU domain XML: a qcow2 disk backed
/// by the resolved path, virtio net/disk for reasonable default
/// performance, and a Cirrus/VNC-free console setup (console access is
/// this backend's `capabilities().console` claim, exercised through
/// libvirt's own serial/graphics device model — the *choice* of graphics
/// device is intentionally minimal here since Unit 14 (`lsbx-stream`) owns
/// the noVNC/WebSocket proxy layer on top of whatever libvirt exposes, not
/// this unit).
///
/// ### Pubkey injection — documented gap, not silently dropped
/// `req.pubkey` is **not yet injected into the guest** by this function.
/// Doing that properly needs one of: (a) cloud-init (a `NoCloud`
/// ISO/`<metadata>` seed disk with a `user-data` that appends the pubkey
/// to `authorized_keys`), or (b) the golden image already having a
/// provisioning-time mechanism that reads a fixed location (a virtio-9p
/// mount, a `qemu-guest-agent` file-write call) for the key. Both are real
/// engineering, not a one-line omission, and building either fully is
/// judged out of scope for this unit — full cloud-init/guest-agent
/// integration is exactly the kind of "how does key material actually get
/// into a booted guest" question Unit 08 (which owns what a golden's build
/// process produces) and/or a dedicated follow-up unit should settle, not
/// something this backend should invent unilaterally. The pubkey is
/// threaded through as a domain XML `<metadata>` comment (inert to
/// libvirt/QEMU, but keeps the value visible on the domain for a human or
/// a later automated step to act on) so it is not silently discarded
/// end-to-end, and this gap is called out explicitly in the PR description.
pub fn render_domain_xml(params: &DomainXmlParams<'_>, pubkey: &str) -> Result<String, LsbxError> {
    let memory_kib = parse_memory_to_kib(params.memory)?;
    let name = xml_escape(params.name);
    let disk_path = xml_escape(&params.disk_path.to_string_lossy());
    let pubkey_escaped = xml_escape(pubkey);

    Ok(format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='KiB'>{memory_kib}</memory>
  <currentMemory unit='KiB'>{memory_kib}</currentMemory>
  <vcpu placement='static'>{cpu}</vcpu>
  <os>
    <type arch='x86_64' machine='q35'>hvm</type>
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough'/>
  <!-- Guest SSH pubkey injection is a documented gap for this unit — see
       domain_xml::render_domain_xml's doc comment. Recorded here so the
       value is visible on the domain rather than silently dropped
       end-to-end; this metadata node has no effect on QEMU/libvirt. -->
  <metadata>
    <lsbx:pubkey xmlns:lsbx="https://lufs.org/lsbx/domain-metadata">{pubkey_escaped}</lsbx:pubkey>
  </metadata>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='{disk_path}'/>
      <target dev='vda' bus='virtio'/>
    </disk>
    <interface type='network'>
      <source network='default'/>
      <model type='virtio'/>
    </interface>
    <console type='pty'>
      <target type='serial' port='0'/>
    </console>
    <graphics type='vnc' port='-1' autoport='yes'/>
  </devices>
</domain>"#,
        name = name,
        memory_kib = memory_kib,
        cpu = params.cpu,
        pubkey_escaped = pubkey_escaped,
        disk_path = disk_path,
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
    fn parses_megabyte_suffix() {
        assert_eq!(parse_memory_to_kib("512M").unwrap(), 512 * 1024);
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
        };
        let xml = render_domain_xml(&params, "ssh-ed25519 AAAA... lsbx:test").unwrap();
        assert!(xml.contains("<name>lsbx-test-vm</name>"));
        assert!(xml.contains("vcpu placement='static'>4<"));
        assert!(xml.contains("/var/lib/lsbx/vms/lsbx-test-vm.qcow2"));
        assert!(xml.contains("1048576")); // 1G in KiB
    }

    #[test]
    fn domain_xml_escapes_unsafe_characters_in_name_and_pubkey() {
        let params = DomainXmlParams {
            name: "lsbx-<injected>",
            cpu: 1,
            memory: "512M",
            disk_path: std::path::Path::new("/tmp/x.qcow2"),
        };
        let xml = render_domain_xml(&params, "ssh-ed25519 AAAA\"quote lsbx:test").unwrap();
        assert!(!xml.contains("<name>lsbx-<injected></name>"));
        assert!(xml.contains("&lt;injected&gt;"));
        assert!(xml.contains("&quot;quote"));
    }

    #[test]
    fn domain_xml_propagates_invalid_memory_error() {
        let params = DomainXmlParams {
            name: "lsbx-test-vm",
            cpu: 1,
            memory: "not-a-number",
            disk_path: std::path::Path::new("/tmp/x.qcow2"),
        };
        assert!(render_domain_xml(&params, "irrelevant").is_err());
    }
}
