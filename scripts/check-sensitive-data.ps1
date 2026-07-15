[CmdletBinding()]
param(
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

$rules = @(
    [pscustomobject]@{
        Name = 'private-key'
        Pattern = [regex]::new('-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----', 'IgnoreCase,CultureInvariant')
    },
    [pscustomobject]@{
        Name = 'openai-anthropic-key'
        Pattern = [regex]::new('(?<![A-Za-z0-9])sk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}', 'CultureInvariant')
    },
    [pscustomobject]@{
        Name = 'github-token'
        Pattern = [regex]::new('(?<![A-Za-z0-9])(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{30,}', 'CultureInvariant')
    },
    [pscustomobject]@{
        Name = 'github-pat'
        Pattern = [regex]::new('(?<![A-Za-z0-9])github_pat_[A-Za-z0-9_]{40,}', 'CultureInvariant')
    },
    [pscustomobject]@{
        Name = 'slack-token'
        Pattern = [regex]::new('(?<![A-Za-z0-9])xox[baprs]-[A-Za-z0-9-]{20,}', 'CultureInvariant')
    },
    [pscustomobject]@{
        Name = 'google-api-key'
        Pattern = [regex]::new('(?<![A-Za-z0-9])AIza[0-9A-Za-z_-]{30,}', 'CultureInvariant')
    },
    [pscustomobject]@{
        Name = 'credential-assignment'
        Pattern = [regex]::new('(?im)\b(?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password)\b\s*[:=]\s*["'']?[A-Za-z0-9_./+=-]{24,}', 'CultureInvariant')
    }
)

function Find-SensitiveMatches {
    param([Parameter(Mandatory = $true)][string]$Text)

    foreach ($rule in $rules) {
        foreach ($match in $rule.Pattern.Matches($Text)) {
            [pscustomobject]@{
                Rule = $rule.Name
                Index = $match.Index
            }
        }
    }
}

function Test-ScannerRules {
    $samples = @(
        ('ghp_' + ('A' * 36)),
        ('sk-' + ('B' * 32)),
        ('AIza' + ('C' * 35)),
        ('-----BEGIN ' + 'PRIVATE KEY-----'),
        ('access_token' + ' = ' + ('D' * 32))
    )
    foreach ($sample in $samples) {
        if (@(Find-SensitiveMatches -Text $sample).Count -eq 0) {
            throw 'Sensitive-data scanner self-test failed to detect a synthetic fixture.'
        }
    }
    if (@(Find-SensitiveMatches -Text 'fixture-safe-value').Count -ne 0) {
        throw 'Sensitive-data scanner self-test flagged a clean fixture.'
    }
    Write-Output 'Sensitive-data scanner self-test passed.'
}

if ($SelfTest) {
    Test-ScannerRules
}

$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$repoRoot = [IO.Path]::GetFullPath((& git -C $projectRoot rev-parse --show-toplevel).Trim())
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($repoRoot)) {
    throw 'Could not locate the Git worktree root.'
}
$repoPrefix = $repoRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
if (-not $projectRoot.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Project root is outside the Git worktree.'
}
$projectRelative = $projectRoot.Substring($repoPrefix.Length).TrimStart([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar).Replace('\', '/')
$tracked = @(& git -C $repoRoot ls-files --cached --others --exclude-standard -- $projectRelative)
if ($LASTEXITCODE -ne 0) {
    throw 'Could not enumerate project files for sensitive-data scanning.'
}

$findings = @()
foreach ($relative in $tracked) {
    $fullPath = Join-Path $repoRoot $relative
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        continue
    }
    $file = Get-Item -LiteralPath $fullPath
    if ($file.Length -gt 5MB) {
        continue
    }
    $bytes = [IO.File]::ReadAllBytes($fullPath)
    if ($bytes -contains 0) {
        continue
    }
    $text = [Text.Encoding]::UTF8.GetString($bytes)
    foreach ($match in Find-SensitiveMatches -Text $text) {
        $line = 1 + [regex]::Matches($text.Substring(0, $match.Index), "`n").Count
        $display = $relative.Replace('\', '/')
        if ($display.StartsWith($projectRelative + '/', [StringComparison]::OrdinalIgnoreCase)) {
            $display = $display.Substring($projectRelative.Length + 1)
        }
        $findings += [pscustomobject]@{
            Path = $display
            Line = $line
            Rule = $match.Rule
        }
    }
}

if ($findings.Count -gt 0) {
    $findings | Sort-Object Path, Line, Rule | Format-Table -AutoSize | Out-String | Write-Error
    throw "Sensitive-data scan found $($findings.Count) possible tracked secret(s). Values were not printed."
}

Write-Output "Sensitive-data scan passed for $($tracked.Count) project file(s)."
