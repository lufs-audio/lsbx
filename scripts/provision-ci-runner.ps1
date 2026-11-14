# Provision a GitHub-App-authenticated ephemeral CI runner on Windows (Issue #39).

param(
    [string]$EnvFile = "C:\lsbx-ci-broker\lsbx-runner.env"
)

$ErrorActionPreference = "Stop"

# 1. Fix MTU to 1280 to prevent PMTU packet drops over the libvirt bridge
Set-NetIPInterface -InterfaceAlias Ethernet -NlMtuBytes 1280 -ErrorAction SilentlyContinue | Out-Null

# 2. Parse Environment
if (Test-Path $EnvFile) {
    Get-Content $EnvFile | ForEach-Object {
        if ($_ -match '^\s*([A-Za-z0-9_]+)\s*=\s*"?([^"]*)"?\s*$') {
            [System.Environment]::SetEnvironmentVariable($matches[1], $matches[2])
        }
    }
}

$AppId = [System.Environment]::GetEnvironmentVariable("GITHUB_APP_ID")
$AppKey = [System.Environment]::GetEnvironmentVariable("GITHUB_APP_KEY")
$Scope = [System.Environment]::GetEnvironmentVariable("GITHUB_SCOPE")
if (-not $Scope) { $Scope = "org" }
$Owner = [System.Environment]::GetEnvironmentVariable("GITHUB_OWNER")
$Repo = [System.Environment]::GetEnvironmentVariable("GITHUB_REPO")
$RunnerLabels = [System.Environment]::GetEnvironmentVariable("RUNNER_LABELS")
if (-not $RunnerLabels) { $RunnerLabels = "lsbx-windows,windows,carnyx" }
$RunnerGroup = [System.Environment]::GetEnvironmentVariable("RUNNER_GROUP")
if (-not $RunnerGroup) { $RunnerGroup = "continuo" }

if (-not $AppId -or -not $AppKey -or -not $Owner) {
    Write-Error "Missing required GitHub App environment variables"
    exit 1
}

# 3. Mint JWT & Registration Token via Python (using C:\Python312\python.exe)
$pythonScript = @"
import time, json, urllib.request, urllib.parse, base64
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import padding
from cryptography.hazmat.primitives.serialization import load_pem_private_key

app_id = '$AppId'
key_path = r'$AppKey'
owner = '$Owner'
scope = '$Scope'
repo = '$Repo'

now = int(time.time())
iat = now - 60
exp = now + 540

with open(key_path, 'rb') as f:
    private_key = load_pem_private_key(f.read(), password=None)

header = base64.urlsafe_b64encode(b'{"alg":"RS256","typ":"JWT"}').decode().rstrip("=")
payload = base64.urlsafe_b64encode(json.dumps({"iss": app_id, "iat": iat, "exp": exp}).encode()).decode().rstrip("=")
to_sign = f"{header}.{payload}".encode()

sig = private_key.sign(to_sign, padding.PKCS1v15(), hashes.SHA256())
jwt = f"{header}.{payload}." + base64.urlsafe_b64encode(sig).decode().rstrip("=")

# 1. Installation ID
if scope == 'org':
    inst_url = f"https://api.github.com/orgs/{urllib.parse.quote(owner)}/installation"
else:
    inst_url = f"https://api.github.com/repos/{urllib.parse.quote(owner)}/{urllib.parse.quote(repo)}/installation"

req = urllib.request.Request(inst_url, headers={
    'Authorization': f'Bearer {jwt}',
    'Accept': 'application/vnd.github+json',
    'User-Agent': 'lsbx-ci-broker'
})
with urllib.request.urlopen(req) as resp:
    inst_id = json.load(resp)['id']

# 2. Installation Access Token
token_url = f"https://api.github.com/app/installations/{inst_id}/access_tokens"
perms = {'organization_self_hosted_runners': 'write'} if scope == 'org' else {'administration': 'write'}
req = urllib.request.Request(token_url, data=json.dumps({'permissions': perms}).encode(), headers={
    'Authorization': f'Bearer {jwt}',
    'Accept': 'application/vnd.github+json',
    'User-Agent': 'lsbx-ci-broker'
})
with urllib.request.urlopen(req) as resp:
    token = json.load(resp)['token']

# 3. Registration Token
if scope == 'org':
    reg_url = f"https://api.github.com/orgs/{urllib.parse.quote(owner)}/actions/runners/registration-token"
else:
    reg_url = f"https://api.github.com/repos/{urllib.parse.quote(owner)}/{urllib.parse.quote(repo)}/actions/runners/registration-token"

req = urllib.request.Request(reg_url, data=b'', headers={
    'Authorization': f'Bearer {token}',
    'Accept': 'application/vnd.github+json',
    'User-Agent': 'lsbx-ci-broker'
})
with urllib.request.urlopen(req) as resp:
    reg_token = json.load(resp)['token']

print(reg_token)
"@

$regToken = & "C:\Python312\python.exe" -c $pythonScript
if (-not $regToken) {
    Write-Error "Failed to obtain GitHub Actions runner registration token"
    exit 1
}

# 4. Ensure Runner Directory Exists
$runnerDir = "C:\actions-runner"
if (-not (Test-Path "$runnerDir\config.cmd")) {
    New-Item -ItemType Directory -Force -Path $runnerDir | Out-Null
    curl.exe -fSL -o "$runnerDir\runner.zip" "https://github.com/actions/runner/releases/download/v2.337.0/actions-runner-win-x64-2.337.0.zip"
    tar.exe -xf "$runnerDir\runner.zip" -C $runnerDir
    Remove-Item "$runnerDir\runner.zip" -Force
}

# 5. Clean Previous Credentials
Remove-Item -Path "$runnerDir\.runner", "$runnerDir\.credentials*" -Force -ErrorAction SilentlyContinue

# 6. Generate Runner Name
$hostPrefix = [System.Environment]::GetEnvironmentVariable("RUNNER_HOST_PREFIX")
if (-not $hostPrefix) { $hostPrefix = "carnyx" }
$randomHex = [System.Guid]::NewGuid().ToString().Substring(0, 8)
$runnerName = "lsbx-$hostPrefix-win-$randomHex"

# 7. Configure Runner
Set-Location $runnerDir
$configArgs = @(
    "--url", "https://github.com/$Owner",
    "--token", $regToken.Trim(),
    "--name", $runnerName,
    "--labels", $RunnerLabels,
    "--runnergroup", $RunnerGroup,
    "--ephemeral",
    "--unattended"
)
& "$runnerDir\config.cmd" @configArgs

# 8. Output Lifecycle Marker (captured by lsbx-broker)
Write-Host "Runner registered: $runnerName"

# 9. Start Runner Listener in Background
Start-Process -FilePath "$runnerDir\bin\Runner.Listener.exe" `
    -ArgumentList "run" `
    -WorkingDirectory $runnerDir `
    -RedirectStandardOutput "$runnerDir\runner.log" `
    -RedirectStandardError "$runnerDir\runner.err" `
    -WindowStyle Hidden

Write-Host "Runner started in background."
