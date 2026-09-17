# Phase Spec — Hardened Kali Linux + Tor Golden Image

**Timestamp:** `2026-09-17T2114Z`  
**Slug:** `kali-tor-golden`  
**Branch (repo of record):** `feature/kali-tor-golden` in `~/repos/lsbx`  
**Target:** Carnyx libvirt host (`qemu:///system`, local KVM x86_64)

---

## 1. Problem & Executive Summary

For hands-on CompTIA Security+ (SY0-701) study and cybersecurity research, learners require an isolated, disposable offensive-and-defensive Linux environment. Traditional local virtual machines have several drawbacks:
1. **Persistent State & Clutter:** Manual installs accumulate state, break dependencies, and persist credentials.
2. **Exposure & Location Tracking:** Outbound reconnaissance or vulnerability queries directly expose the operator's public residential or office IP address and ISP geolocation.
3. **Hypervisor Drift:** Ad-hoc VMs are disconnected from the standardized `lsbx` disposable-VM orchestration, preventing automated teardown, leased lifetimes, and scripted agent interactions.
4. **Host Safety Risks:** Carnyx runs a production GitHub Actions CI broker (`lsbx-ci-broker.service`) on the shared libvirt NAT bridge (`virbr0`). Naive attempts to route the entire hypervisor bridge through Tor would torify CI runner traffic, triggering immediate GitHub API rate-limits and HTTP 403 blocks on runner dispatches.

This phase implements a dedicated, hardened **`kali`** golden image within `lsbx`, featuring:
- A compact, high-performance base built on official Kali Linux Cloud GenericCloud (`amd64`).
- Core CompTIA Security+ offensive/defensive toolsets (`kali-tools-top10`, `nikto`, `gobuster`, `tcpdump`, `wireshark`/`tshark`).
- **Dual-layer Tor anonymity**: Per-tool SOCKS5 via `proxychains4` and transparent system-wide outbound Tor routing via `/usr/local/bin/tor-route`, with an **RFC 1918 bypass rule** ensuring `lsbx` host-to-guest SSH (`192.168.122.X:22`) remains completely undisturbed.
- Host-level and guest-level hardening (UFW firewall, sysctl kernel parameters, SSH key-only auth).
- Fully **ephemeral credentials**: Dynamic keypair generation per VM lifecycle using cloud-init `cidata` seed ISOs.
- Codebase-wide transition of the default guest username from legacy `"exedev"` to `"lsbx"`.

---

## 2. Technical Reconnaissance & Architecture

### 2.1 Libvirt & `lsbx` Storage on Carnyx
- **Golden images repository:** `/home/carnyx/ISOs/images/goldens/`
  - Golden disk created: `lsbx-kali-v1.qcow2` (3.6 GB, standalone compressed qcow2).
  - Golden symlink: `kali.qcow2 -> lsbx-kali-v1.qcow2` (matches `lsbx-backend-libvirt::golden_disk` convention).
- **Per-VM Working Disks:** `/home/carnyx/ISOs/images/work/` (COW overlays linked to golden backing file).
- **Manifests Updated:** `images.carnyx.json` and `images.json`.

### 2.2 Domain XML & Cloud-Init Lifecycle
In `crates/lsbx-backend-libvirt/src/domain_xml.rs` and `lib.rs`:
- Every Linux domain attaches a virtio NIC to libvirt's `default` network (`virbr0`, `192.168.122.0/24`).
- When `lsbx up kali` is invoked, `create_seed_iso` generates a FAT ISO (`cidata`) with:
  ```yaml
  #cloud-config
  users:
    - name: lsbx
      sudo: ALL=(ALL) NOPASSWD:ALL
      shell: /bin/bash
      ssh_authorized_keys:
        - <ephemeral_ed25519_pubkey>
  ssh_pwauth: false
  ```
- The guest boots, cloud-init configures user `lsbx`, and `qemu-guest-agent` resolves the guest IPv4 address.
- `lsbx exec` executes commands over batch-mode SSH (`ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null -i <ephemeral_key> lsbx@<guest_ip> <cmd>`).

### 2.3 Host Safety & Tor Isolation Mechanism
Carnyx runs `lsbx-ci-broker.service` on `virbr0`. Modifying `virbr0` at the host level is strictly forbidden.
Instead, **guest-internal transparent Tor routing** was implemented:
- In-guest Tor daemon listens on `127.0.0.1:9040` (`TransPort`), `127.0.0.1:5353` (`DNSPort`), and `127.0.0.1:9050` (`SocksPort`).
- Outbound routing table (`/usr/local/bin/tor-route`):
  1. Return Tor's own UID traffic (`debian-tor`) to prevent circular routing.
  2. Return loopback (`127.0.0.0/8`) and local RFC 1918 traffic (`192.168.0.0/16`, `10.0.0.0/8`, `172.16.0.0/12`) directly without redirection.
  3. Redirect all outbound UDP port 53 (DNS) queries to Tor's `DNSPort` (`5353`).
  4. Redirect all outbound TCP SYN packets to Tor's `TransPort` (`9040`).

Result: Public internet traffic from the guest is scrubbed of original geolocation and masked behind Tor exit nodes, while hypervisor communication from Carnyx (`192.168.122.1`) into the guest (`192.168.122.X`) functions seamlessly.

---

## 3. Package Selection: Kali Metapackages

Kali Linux packages hundreds of specialized offensive tools. Evaluation of package sets:
- **`kali-linux-everything`**: Includes >600 tools, SDR radio drivers, massive wordlists, and legacy exploits. Demands 35–50 GB disk space and takes hours to build.
- **`kali-tools-top10` + targeted tools (Selected)**:
  - `aircrack-ng`, `burpsuite`, `hydra`, `john`, `metasploit-framework`, `netexec`, `nmap`, `responder`, `sqlmap`, `wireshark`/`tshark`.
  - Added `nikto` (web server scanner), `gobuster` (URI/DNS enumeration), `tcpdump`, `socat`, `netcat-traditional`, `dnsutils`.
  - Directly matches CompTIA Security+ domains without multi-gigabyte bloat.

---

## 4. Hardening Profile Applied

1. **Kernel Sysctl Hardening (`/etc/sysctl.d/99-secplus-hardened.conf`)**:
   - `net.ipv4.conf.all.rp_filter = 1` (Anti-IP spoofing)
   - `net.ipv4.tcp_syncookies = 1` (SYN flood defense)
   - `net.ipv4.conf.all.accept_source_route = 0` (Disable loose source routing)
   - `net.ipv4.conf.all.accept_redirects = 0` (Disable ICMP redirect poisoning)
   - `net.ipv4.icmp_echo_ignore_broadcasts = 1` (Mitigate Smurf attacks)
   - `kernel.randomize_va_space = 2` (Full Address Space Layout Randomization)
   - `kernel.yama.ptrace_scope = 1` (Restrict cross-process ptrace inspection)
   - `kernel.dmesg_restrict = 1` (Prevent unprivileged kernel log leakage)

2. **Host-Based Firewall (`ufw`)**:
   - Default incoming policy: `DENY`
   - Default outgoing policy: `ALLOW`
   - Loopback (`lo`): `ALLOW`
   - SSH inbound (`tcp/22`): Restricted strictly to `192.168.122.0/24` (libvirt hypervisor bridge). All other unsolicited inbound traffic is dropped.

3. **SSH Daemon Lockdown (`/etc/ssh/sshd_config.d/99-lsbx-hardened.conf`)**:
   - `PasswordAuthentication no`
   - `PermitRootLogin prohibit-password`
   - `PubkeyAuthentication yes`
   - `X11Forwarding no`
   - `MaxAuthTries 4`

---

## 5. Codebase Changes

### 5.1 Default Username Migration (`exedev` -> `lsbx`)
- `crates/lsbx-backend-libvirt/src/lib.rs`: Default `guest_username` updated from `"exedev"` to `"lsbx"`.
- `crates/lsbx-cli/src/lib.rs`: Username resolution fallback updated from `"exedev"` to `"lsbx"`.
- Both `lsbx` and `exedev` remain present in `/etc/sudoers.d/99-lsbx` inside the golden image for backwards compatibility with legacy tooling.

### 5.2 Auto-Probe Integration Test Fix
- `crates/lsbx-cli/tests/test_backend_auto_probe.rs`: Updated `backend_auto_falls_through_to_demo_when_nothing_else_is_available` to explicitly isolate `LSBX_LIBVIRT_URI` (`qemu+unix:///nonexistent?socket=/nonexistent`), allowing the test to pass on live libvirt hosts like Carnyx without falsely succeeding on active hypervisors.

### 5.3 Golden Registry Manifests
- `images.carnyx.json` and `images.json`: Added `kali` golden entry:
  ```json
  {
    "key": "kali",
    "flavor": "agent",
    "os": "linux",
    "base": "lsbx-kali-v1",
    "mode": "copy",
    "cpu": 2,
    "memory": "4GB",
    "streaming": "none",
    "capabilities": [
      "security",
      "tor"
    ],
    "healthcheck": [
      "curl --version",
      "nmap --version",
      "systemctl is-active tor",
      "curl -fsS --retry 6 --retry-delay 3 --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/api/ip"
    ],
    "description": "Kali Linux + Tor"
  }
  ```
  Profile registered: `"kali": { "golden": "kali" }`.

---

## 6. Acceptance & Verification Evidence

1. **Workspace Compilation & Linters**:
   - `cargo check --workspace` — Passed cleanly.
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings` — Passed cleanly, 0 warnings.
   - `cargo test --workspace` — All unit and integration tests passed across all 17 crates.

2. **Golden Verification (`lsbx golden verify kali`)**:
   ```
   COMMAND                                                                                                   RESULT
   curl --version                                                                                            pass  
   nmap --version                                                                                            pass  
   systemctl is-active tor                                                                                   pass  
   curl -fsS --retry 6 --retry-delay 3 --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/api/ip  pass  
   ```

3. **Live VM Lifecycle (`lsbx up`, `lsbx exec`, `lsbx down`)**:
   - VM booted cleanly, ephemeral ED25519 credentials exchanged via cloud-init.
   - `whoami` proved user `lsbx` without setting `LSBX_LIBVIRT_USER`.
   - Tested Tor SOCKS check: `{"IsTor": true, "IP": "107.189.8.181"}`.
   - Tested transparent routing (`tor-route on` + `curl https://check.torproject.org/api/ip`): `{"IsTor": true, "IP": "185.220.101.16"}`.
   - Tested tools: `nmap 7.99`, `john 1.9.0-jumbo`, `sqlmap 1.10.8#stable`, UFW rules, sysctl parameters.
   - Teardown cleanly undefines domain and deletes storage overlay.
