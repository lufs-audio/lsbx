# User & Operator Guide: Kali Linux + Tor Golden Image

**Golden Key:** `kali`  
**Profile:** `kali`  
**Default Specs:** 2 vCPUs, 4 GB RAM, 25 GB root storage (COW overlay)  
**Default Guest User:** `lsbx` (with passwordless `sudo`)

---

## 1. Quick Start

### 1.1 Spin Up a Disposable Sandbox

To launch a temporary Kali VM with a 2-hour lease:

```bash
# Ensure libvirt paths are configured (or set in environment)
export LSBX_LIBVIRT_IMAGES_DIR=/home/carnyx/ISOs/images/goldens
export LSBX_LIBVIRT_VM_DIR=/home/carnyx/ISOs/images/work
export LIBVIRT_DEFAULT_URI=qemu:///system

# Launch sandbox
lsbx -b libvirt -i images.carnyx.json up kali --lease 2h
```

Output:
```
id                sbx-18d6384bcc0fc7c7-141a3aa6
name              sbx-18d6384bcc0fc7c7-141a3aa6
host              localhost
profile           kali
flavor            agent
streaming         none
created_at        2026-09-17T21:16:49+00:00
lease_expires_at  2026-09-17T23:16:49+00:00
```

### 1.2 Run Commands Inside the Guest

Use `lsbx exec <sandbox-id> -- <command>` to execute commands via ephemeral SSH:

```bash
# Verify current user
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- whoami
# Output: lsbx

# Check Tor routing and external IP
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- tor-route status
```

### 1.3 Terminate the Sandbox

When your study session is finished:

```bash
lsbx -b libvirt -i images.carnyx.json down <sandbox-id>
```

---

## 2. Tor Anonymity Modes

The image provides two distinct ways to route traffic through Tor.

### Mode A: Per-Tool SOCKS5 via Proxychains (Recommended for fine-grained testing)
Kali comes pre-configured with `proxychains4`, configured to use Tor (`127.0.0.1:9050`) with remote DNS resolution to avoid DNS leaks.

Prefix any standard CLI tool with `proxychains4`:
```bash
# Nmap scan through Tor (Note: Must use TCP connect scan -sT and no-ping -Pn)
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- \
  proxychains4 nmap -sT -Pn -p 80,443 scanme.nmap.org

# Curl through Tor
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- \
  proxychains4 curl -s https://check.torproject.org/api/ip

# SQLmap through Tor
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- \
  proxychains4 sqlmap -u "http://testphp.vulnweb.com/artists.php?artist=1" --batch
```

### Mode B: Transparent Outbound Tor Routing
If you want **all** outbound network traffic from every tool and script in the guest to exit via Tor automatically without prefixing commands:

```bash
# Enable transparent Tor routing
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- sudo tor-route on

# Now any regular command is routed through Tor
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- curl -s https://check.torproject.org/api/ip

# Disable transparent Tor routing when finished
lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- sudo tor-route off
```

> **Important Note for Security+ Students:**  
> Tor only transports **TCP** streams. Raw ICMP packets (`ping <host>`) or raw SYN/UDP scans (`nmap -sS` / `nmap -sU`) cannot traverse Tor exit nodes and will fail or be dropped. Always use TCP Connect scans (`-sT`) and skip host discovery (`-Pn`) when testing through Tor.

---

## 3. CompTIA Security+ Lab Use Cases

The tools inside this golden image directly support practical labs across multiple Security+ (SY0-701) domains:

### Domain 1: General Security Concepts & AAA
- **Privilege Separation:** Verify `lsbx` vs `root` execution with `sudo -l`.
- **Password Auditing & Hashes:**
  ```bash
  # Benchmark hash cracking speeds with John the Ripper
  lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- john --test
  ```

### Domain 2: Threats, Vulnerabilities & Mitigations
- **Port Scanning & Service Enumeration:**
  ```bash
  lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- \
    proxychains4 nmap -sT -Pn -sV --version-light -p 80,443 scanme.nmap.org
  ```
- **Web Application Vulnerability Discovery:**
  ```bash
  lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- \
    proxychains4 nikto -h http://testphp.vulnweb.com
  ```
- **Directory & Resource Brute-Forcing:**
  ```bash
  lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- \
    proxychains4 gobuster dir -u http://testphp.vulnweb.com -w /usr/share/wordlists/dirb/common.txt
  ```

### Domain 3: Security Architecture & Network Defense
- **Firewall & Ingress Inspection:**
  ```bash
  # Inspect UFW active rules
  lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- sudo ufw status verbose
  ```
- **Packet Capture & Protocol Analysis:**
  ```bash
  # Capture 5 packets on loopback or virtual interface using tshark
  lsbx -b libvirt -i images.carnyx.json exec <sandbox-id> -- \
    tshark -c 5 -i lo
  ```

---

## 4. Verification & Healthcheck Maintenance

To verify that the golden image on disk remains healthy and bootable:

```bash
lsbx -b libvirt -i images.carnyx.json golden verify kali
```

This runs:
1. `curl --version`
2. `nmap --version`
3. `systemctl is-active tor`
4. `curl -fsS --retry 6 --retry-delay 3 --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/api/ip`

All checks must return `pass`.
