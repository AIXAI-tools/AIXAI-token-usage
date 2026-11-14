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
        $parts = @()
        if ($null -ne $limits.five_hour) { $parts += '5h ' + $limits.five_hour.used_percentage + '%' }
        if ($null -ne $limits.seven_day) { $parts += '本週 ' + $limits.seven_day.used_percentage + '%' }
        if ($parts.Count -gt 0) { [Console]::WriteLine('Claude ' + ($parts -join '｜')) }
    }
} catch {}
exit 0
