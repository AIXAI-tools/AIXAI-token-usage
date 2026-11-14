param([string]$Key, [string]$Dir)

$ErrorActionPreference = 'Stop'
try {
    [Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    $raw = [Console]::In.ReadToEnd()
    $data = $null
    try { $data = $raw | ConvertFrom-Json } catch {}
    $limits = $data.rate_limits
    if ($null -ne $limits.five_hour -or $null -ne $limits.seven_day) {
        try {
            $email = $null
            $emailFile = Join-Path $Dir '.claude.json'
            if ($Dir.TrimEnd('\', '/') -eq (Join-Path $env:USERPROFILE '.claude')) {
                $emailFile = Join-Path $env:USERPROFILE '.claude.json'
            }
            # .claude.json often has keys differing only by case; PowerShell 5.1 ConvertFrom-Json rejects them, so only regex out oauthAccount.emailAddress
            try {
                $text = [IO.File]::ReadAllText($emailFile)
                $m = [regex]::Match($text, '"oauthAccount"\s*:\s*\{[^{}]*?"emailAddress"\s*:\s*"([^"\\]+)"')
                if ($m.Success) { $email = $m.Groups[1].Value }
            } catch {}
            $status = [ordered]@{
                v = 1
                captured_at = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
                email = $email
            }
            foreach ($window in @('five_hour', 'seven_day')) {
                if ($null -ne $limits.$window) { $status[$window] = $limits.$window }
            }
            $folder = Join-Path $PSScriptRoot 'claude-status'
            [IO.Directory]::CreateDirectory($folder) | Out-Null
            $path = Join-Path $folder ($Key + '.json')
            [IO.File]::WriteAllText(($path + '.tmp'), ($status | ConvertTo-Json -Depth 20 -Compress), (New-Object System.Text.UTF8Encoding($false)))
            Move-Item -LiteralPath ($path + '.tmp') -Destination $path -Force
        } catch {}
    }
    $config = $null
    try { $config = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'statusline.json')) | ConvertFrom-Json } catch {}
    $prev = $config.prev.$Key
    if ($prev) {
        try {
            $info = New-Object System.Diagnostics.ProcessStartInfo
            $info.FileName = 'cmd.exe'
            $info.Arguments = '/d /s /c "' + $prev + '"'
            $info.UseShellExecute = $false
            $info.CreateNoWindow = $true
            $info.RedirectStandardInput = $true
            $info.RedirectStandardOutput = $true
            $info.RedirectStandardError = $true
            $process = New-Object System.Diagnostics.Process
            $process.StartInfo = $info
            $process.Start() | Out-Null
            $stdout = $process.StandardOutput.BaseStream.CopyToAsync([Console]::OpenStandardOutput())
            $stderr = $process.StandardError.ReadToEndAsync()
            $bytes = [Text.Encoding]::UTF8.GetBytes($raw)
            $process.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
            $process.StandardInput.BaseStream.Close()
            [void]$stdout.GetAwaiter().GetResult()
            $process.WaitForExit()
            $process.Dispose()
        } catch {}
    } elseif ($config.show -eq $true) {
        # terminal-line.json is written by AItoken after each refresh: labels for the Claude part plus the Codex/agy text
        $term = $null
        try { $term = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'terminal-line.json')) | ConvertFrom-Json } catch {}
        if ($null -eq $term) {
            $parts = @()
            if ($null -ne $limits.five_hour) { $parts += '5h ' + $limits.five_hour.used_percentage + '%' }
            if ($null -ne $limits.seven_day) { $parts += '本週 ' + $limits.seven_day.used_percentage + '%' }
            if ($parts.Count -gt 0) { [Console]::WriteLine('Claude ' + ($parts -join '｜')) }
        } else {
            $esc = [char]27

            $pieces = @()
            if ($term.claude -ne $false) {
                $parts = @()
                foreach ($name in @('five_hour', 'seven_day')) {
                    $w = $limits.$name
                    if ($null -eq $w -or $null -eq $w.used_percentage) { continue }
                    $used = [double]$w.used_percentage
                    $left = 100 - $used
                    $shown = $left
                    if ($term.used -eq $true) { $shown = $used }
                    $code = '32'
                    if ($left -lt 10) { $code = '31' } elseif ($left -lt 30) { $code = '33' }
                    $label = $term.lw
                    if ($name -eq 'five_hour') { $label = $term.l5 }
                    $num = [math]::Round($shown, [MidpointRounding]::AwayFromZero)
                    $parts += $label + ' ' + $esc + '[' + $code + 'm' + $num + '%' + $esc + '[0m'
                }
                if ($parts.Count -gt 0) { $pieces += [pscustomobject]@{ s = ''; t = ($term.claude_label + ' ' + ($parts -join $term.dot)); c = '' } }
            }
            $age = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - [int64]$term.updated_at
            $fresh = $age -le 900
            if ($null -ne $term.pieces) {
                $more = $term.pieces_stale
                if ($fresh) { $more = $term.pieces }
                foreach ($p in $more) { $pieces += $p }
            } else {
                $extra = $term.extra_stale
                if ($fresh) { $extra = $term.extra }
                if ($extra) { $pieces += [pscustomobject]@{ s = $term.join; t = $extra; c = '' } }
            }
            # Wrap only between pieces when the terminal is too narrow (COLUMNS is set by Claude Code; CJK counts as 2 cells)
            $max = 0
            try { $max = [int]$env:COLUMNS - 4 } catch {}
            if ($max -le 0) { $max = [int]::MaxValue }
            $ansi = [regex]($esc + '\[[0-9;]*m')
            $lines = @()
            $cur = ''
            $curW = 0
            foreach ($p in $pieces) {
                if ($cur -eq '') { $add = $p.t } else { $add = [string]$p.s + $p.t }
                $w = 0
                foreach ($ch in $ansi.Replace($add, '').ToCharArray()) { if ([int]$ch -ge 0x2E80) { $w += 2 } else { $w += 1 } }
                if ($cur -ne '' -and $curW + $w -gt $max) {
                    $lines += $cur
                    $cur = [string]$p.c + $p.t
                    $curW = 0
                    foreach ($ch in $ansi.Replace($cur, '').ToCharArray()) { if ([int]$ch -ge 0x2E80) { $curW += 2 } else { $curW += 1 } }
                } else {
                    $cur += $add
                    $curW += $w
                }
            }
            if ($cur -ne '') { $lines += $cur }
            foreach ($line in $lines) { [Console]::WriteLine($line) }
        }
    }
} catch {}
exit 0
