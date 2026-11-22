# Initializes/repairs developer-only .env settings. Windows PowerShell 5.1 compatible.
param(
    [Parameter(Mandatory=$true)][string]$ProjectRoot,
    [switch]$LocalAI
)
$ErrorActionPreference = 'Stop'
$envPath = Join-Path $ProjectRoot '.env'
$keys = @('EMPLOYEE_API_TOKEN','REVIEWER_API_TOKEN','SQLITE_PATH','BIND_ADDR','AI_MODE',
          'OLLAMA_BASE_URL','OLLAMA_MODEL','AZURE_OPENAI_ENDPOINT',
          'AZURE_OPENAI_DEPLOYMENT','AZURE_OPENAI_API_KEY')
$settings = @{}
$unmodified = New-Object 'System.Collections.Generic.List[string]'
if (Test-Path -LiteralPath $envPath) {
    foreach ($line in [System.IO.File]::ReadAllLines($envPath)) {
        if ($line -match '^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$') {
            $key = $matches[1].ToUpperInvariant()
            $value = $matches[2].Trim().Trim([char[]]@([char]34, [char]39))
            if ($keys -contains $key) {
                $settings[$key] = $value
                continue
            }
        }
        $unmodified.Add($line)
    }
}
function New-Token {
    $bytes = New-Object byte[] 24
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
    return [BitConverter]::ToString($bytes).Replace('-', '').ToLowerInvariant()
}
function Invalid-Token([string]$token) {
    return [string]::IsNullOrWhiteSpace($token) -or $token.Length -lt 16 -or
           $token.StartsWith('REPLACE_', [StringComparison]::OrdinalIgnoreCase) -or
           $token.StartsWith('YOUR_', [StringComparison]::OrdinalIgnoreCase)
}
$repaired = $false
foreach ($key in @('EMPLOYEE_API_TOKEN','REVIEWER_API_TOKEN')) {
    if (Invalid-Token $settings[$key]) {
        $settings[$key] = New-Token
        $repaired = $true
    }
}
if ($settings['EMPLOYEE_API_TOKEN'] -eq $settings['REVIEWER_API_TOKEN']) {
    $settings['REVIEWER_API_TOKEN'] = New-Token
    $repaired = $true
}
if (-not $settings['SQLITE_PATH']) { $settings['SQLITE_PATH'] = './reviewflow.db' }
if (-not $settings['BIND_ADDR']) { $settings['BIND_ADDR'] = '127.0.0.1:8080' }
if (-not $settings['AI_MODE']) { $settings['AI_MODE'] = 'rules' }
if (-not $settings['OLLAMA_MODEL']) { $settings['OLLAMA_MODEL'] = 'qwen3:4b-instruct' }
if (-not $settings['OLLAMA_BASE_URL']) { $settings['OLLAMA_BASE_URL'] = 'http://127.0.0.1:11434' }
if ($LocalAI) { $settings['AI_MODE'] = 'ollama' }

# Rewrite recognized keys once, in a consistent syntax readable by batch and Docker.
foreach ($key in $keys) {
    if ($settings.ContainsKey($key)) { $unmodified.Add(('{0}={1}' -f $key, $settings[$key])) }
}
$tempPath = "$envPath.tmp"
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllLines($tempPath, $unmodified, $utf8NoBom)
Move-Item -LiteralPath $tempPath -Destination $envPath -Force
if ($repaired) { Write-Host '[ReviewFlow] Repaired missing or placeholder local API tokens in .env.' }
if ($LocalAI) { Write-Host '[ReviewFlow] Enabled local Ollama AI mode.' }
Write-Host '[ReviewFlow] Configuration ready. Existing valid tokens preserved.'
