# Roadmap & Next Steps: Kali Linux + Tor Golden Image

This document outlines future enhancements, operational milestones, and architectural improvements planned for the Kali Linux + Tor ecosystem in `lsbx`.

---

## 1. Near-Term Milestones (v1.1)

### 1.1 Browser-Based Desktop Streaming (noVNC / XFCE)
- **Current State:** The `kali` image is configured with `flavor: agent` (headless CLI, OpenSSH access).
- **Target:** Introduce a companion `kali-desktop` profile or enable `streaming: novnc` on port `8000`:
  - Install a lightweight desktop environment (`xfce4`, `xfce4-terminal`).
  - Install `x11vnc` binding to `127.0.0.1:5900` + Python `websockify` on `0.0.0.0:8000`.
  - Allow `8000/tcp` from `192.168.122.0/24` in UFW firewall rules.
  - Enable interactive browser access to GUI tools like Burp Suite Community, Wireshark GUI, and OWASP ZAP via `lsbx up kali-desktop` → `console_url`.

### 1.2 Dedicated Isolated Libvirt Bridge (`virbr-tor` / Whonix Model)
- **Current State:** Libvirt domains hardcode `<source network='default'/>` in `crates/lsbx-backend-libvirt/src/domain_xml.rs`.
- **Target:**
  - Add optional `network: Option<String>` to `GoldenConfig` and `DomainXmlParams`.
  - Create a host-level libvirt isolated network `<network><name>tor-net</name><forward mode='none'/></network>`.
  - Attach Kali sandboxes directly to this isolated network, routing traffic through an independent Whonix-Gateway VM.
  - Benefit: Provides hypervisor-level network isolation where even a rogue root process inside the guest cannot bypass Tor by flushing local iptables.

---

## 2. Mid-Term Enhancements (v1.2)

### 2.1 Automated Golden Rebuilding Pipeline
- Leverage `lsbx golden build` (`crates/lsbx-golden/src/build.rs`) once the qcow2 flattener in Unit 19 (`lsbx-bootstrap`) is fully integrated.
- Define a reproducible provisioning script in `scripts/build-kali-golden.sh`.
- Run monthly scheduled builds to pull latest Kali package updates, rebuild `lsbx-kali-v<N>.qcow2`, and update the content hash in `images.carnyx.json`.

### 2.2 Security+ Lab Preset Scenarios
- Integrate multi-sandbox lab profiles into `lsbx`:
  - `kali` attacker sandbox paired with an intentional vulnerable target sandbox (e.g., `metasploitable` or `owasp-juice-shop`) on a shared private libvirt bridge.
  - Allows Security+ learners to practice active scanning, exploit demonstration, and remediation in a zero-risk, disposable network segment.

---

## 3. Maintenance & Deprecation Policy

- **Image Archival:** Retain the base image archive (`kali-linux-2026.2-cloud-genericcloud-amd64.tar.xz`) under `/home/carnyx/ISOs/images/build/` for rapid rebuilds.
- **Backing Chains:** Ensure all deployed golden images under `/home/carnyx/ISOs/images/goldens/` are standalone flattened qcow2 files with no dangling external backing references.
- **User Compatibility:** Maintain both `lsbx` and legacy `exedev` passwordless sudo entries in golden `/etc/sudoers.d/99-lsbx` to guarantee compatibility across legacy Python tools and current Rust orchestration.
