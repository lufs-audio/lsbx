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
/// separately (see `crate::golden_disk`) and the golden's `os` string.
pub struct DomainXmlParams<'a> {
    pub name: &'a str,
    pub cpu: u32,
    pub memory: &'a str,
    pub disk_path: &'a std::path::Path,
    /// Optional cloud-init seed ISO path. When `Some`, an IDE cdrom device
    /// is added to the domain XML so the guest can read cloud-init
    /// `user-data`/`meta-data` at boot (SSH key injection, hostname, etc.).
    pub seed_iso: Option<&'a std::path::Path>,
    /// The golden's declared `os`. `"windows"` renders a UEFI-hosted,
    /// SecureBoot-enabled q35 domain with a virtual TPM (mirroring the
    /// `provision-win11` reference domain) — a plain BIOS `pc` machine
    /// cannot boot a Windows 11 guest; every other value renders the
    /// Linux-style BIOS domain below.
    pub os: &'a str,
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
///
/// For `os == "windows"` the Linux template is replaced wholesale with a
/// Windows-shaped domain: q35 (`pc-q35-11.0`, matching this host's
/// provisioned Windows reference — see `provision-win11`), `firmware='efi'`
/// with the OVMF SecureBoot code image and a per-VM `_VARS.fd` nvram cut
/// from the OVMF vars template, the `<hyperv>`/`<smm>` feature block,
/// localtime clock with the hyperv clock timer, and a swtpm 2.0 TPM device.
/// Windows guests do not run cloud-init, so on this path the seed ISO is
/// never attached (callers must pass `seed_iso: None`) and the injected
/// pubkey lives only in `<metadata>` (it must already be baked into the
/// golden's `authorized_keys` for SSH to work at all).
pub fn render_domain_xml(params: &DomainXmlParams<'_>, pubkey: &str) -> Result<String, LsbxError> {
    let memory_kib = parse_memory_to_kib(params.memory)?;
    let name = xml_escape(params.name);
    let disk_path = xml_escape(&params.disk_path.to_string_lossy());
    let pubkey_escaped = xml_escape(pubkey);

    if params.os == "windows" {
        return Ok(render_windows_domain_xml(
            &name, memory_kib, params.cpu, &disk_path, &pubkey_escaped,
        ));
    }

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

/// Renders the Windows-shaped domain (see [`render_domain_xml`]'s doc
/// comment). `name`/`disk_path`/`pubkey_escaped` arrive XML-escaped.
fn render_windows_domain_xml(
    name: &str,
    memory_kib: u64,
    cpu: u32,
    disk_path: &str,
    pubkey_escaped: &str,
) -> String {
    format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='KiB'>{memory_kib}</memory>
  <currentMemory unit='KiB'>{memory_kib}</currentMemory>
  <vcpu placement='static'>{cpu}</vcpu>
  <os firmware='efi'>
    <type arch='x86_64' machine='pc-q35-11.0'>hvm</type>
    <firmware>
      <feature enabled='no' name='enrolled-keys'/>
      <feature enabled='yes' name='secure-boot'/>
    </firmware>
    <loader readonly='yes' secure='yes' type='pflash' format='raw'>/usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd</loader>
    <nvram template='/usr/share/edk2/x64/OVMF_VARS.4m.fd' templateFormat='raw' format='raw'>/var/lib/libvirt/qemu/nvram/{name}_VARS.fd</nvram>
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/>
    <apic/>
    <hyperv mode='custom'>
      <relaxed state='on'/>
      <vapic state='on'/>
      <spinlocks state='on' retries='8191'/>
      <vpindex state='on'/>
      <runtime state='on'/>
      <synic state='on'/>
      <stimer state='on'/>
      <frequencies state='on'/>
      <tlbflush state='on'/>
      <ipi state='on'/>
      <evmcs state='on'/>
      <avic state='on'/>
    </hyperv>
    <vmport state='off'/>
    <smm state='on'/>
  </features>
  <cpu mode='host-passthrough' check='none' migratable='on'/>
  <clock offset='localtime'>
    <timer name='rtc' tickpolicy='catchup'/>
    <timer name='pit' tickpolicy='delay'/>
    <timer name='hpet' present='no'/>
    <timer name='hypervclock' present='yes'/>
  </clock>
  <on_poweroff>destroy</on_poweroff>
  <on_reboot>restart</on_reboot>
  <on_crash>destroy</on_crash>
  <pm>
    <suspend-to-mem enabled='no'/>
    <suspend-to-disk enabled='no'/>
  </pm>
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
    <interface type='network'>
      <source network='default'/>
      <model type='virtio'/>
    </interface>
    <serial type='pty'/>
    <console type='pty'/>
    <channel type='unix'>
      <source mode='bind'/>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <input type='tablet' bus='usb'/>
    <input type='mouse' bus='ps2'/>
    <input type='keyboard' bus='ps2'/>
    <tpm model='tpm-tis'>
      <backend type='emulator' version='2.0'/>
    </tpm>
    <graphics type='vnc' port='-1' autoport='yes'/>
    <video>
      <model type='qxl' ram='65536' vram='65536' vgamem='16384' heads='1' primary='yes'/>
    </video>
  </devices>
</domain>"#,
        name = name,
        memory_kib = memory_kib,
        cpu = cpu,
        disk_path = disk_path,
        pubkey_escaped = pubkey_escaped,
    )
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
            os: "linux",
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
            os: "linux",
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
            os: "linux",
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
            os: "linux",
        };
        assert!(render_domain_xml(&params, "irrelevant").is_err());
    }

    #[test]
    fn windows_domain_uses_uefi_ovmf_secureboot_tpm_and_q35() {
        let params = DomainXmlParams {
            name: "lsbx-win-probe",
            cpu: 4,
            memory: "8GB",
            disk_path: std::path::Path::new("/var/lib/lsbx/vms/lsbx-win-probe.qcow2"),
            seed_iso: Some(std::path::Path::new("/var/lib/lsbx/vms/lsbx-win-probe-cidata.iso")),
            os: "windows",
        };
        let xml = render_domain_xml(&params, "ssh-ed25519 AAAA... lsbx:win").unwrap();
        assert!(xml.contains("machine='pc-q35-11.0'"));
        assert!(xml.contains("firmware='efi'"));
        assert!(xml.contains("/usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd"));
        assert!(xml.contains("/var/lib/libvirt/qemu/nvram/lsbx-win-probe_VARS.fd"));
        assert!(xml.contains("<feature enabled='yes' name='secure-boot'/>"));
        assert!(xml.contains("<hyperv mode='custom'>"));
        assert!(xml.contains("<smm state='on'/>"));
        assert!(xml.contains("<tpm model='tpm-tis'>"));
        assert!(xml.contains("<backend type='emulator' version='2.0'/>"));
        assert!(xml.contains("<clock offset='localtime'>"));
        assert!(xml.contains("<timer name='hypervclock' present='yes'/>"));
        assert!(xml.contains("<model type='qxl'"));
        assert!(xml.contains("<graphics type='vnc' port='-1' autoport='yes'/>"));
        // No cloud-init cdrom even when a seed ISO path is given — Windows
        // never reads it, and attaching it on the IDE bus would waste a
        // boot-order slot.
        assert!(!xml.contains("device='cdrom'"));
        assert!(!xml.contains("<target dev='hda'"));

        // BIOS pc machine must not leak into the Windows shape.
        assert!(!xml.contains("machine='pc'>"));
    }

    #[test]
    fn windows_domain_keeps_memory_and_cpu() {
        let params = DomainXmlParams {
            name: "lsbx-win-mem",
            cpu: 4,
            memory: "8GB",
            disk_path: std::path::Path::new("/var/lib/lsbx/vms/lsbx-win-mem.qcow2"),
            seed_iso: None,
            os: "windows",
        };
        let xml = render_domain_xml(&params, "irrelevant").unwrap();
        assert!(xml.contains("<memory unit='KiB'>8388608</memory>"));
        assert!(xml.contains("<currentMemory unit='KiB'>8388608</currentMemory>"));
        assert!(xml.contains("vcpu placement='static'>4<"));
    }

    #[test]
    fn non_windows_os_renders_bios_guest() {
        let params = DomainXmlParams {
            name: "lsbx-freebsd-probe",
            cpu: 2,
            memory: "1G",
            disk_path: std::path::Path::new("/var/lib/lsbx/vms/lsbx-freebsd-probe.qcow2"),
            seed_iso: None,
            os: "freebsd",
        };
        let xml = render_domain_xml(&params, "ignored").unwrap();
        assert!(xml.contains("machine='pc'>"));
        assert!(!xml.contains("firmware='efi'"));
        assert!(!xml.contains("<tpm"));
    }
}
