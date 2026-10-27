# Windows PowerShell 5.1+; no external runtime or downloader is required.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Repo,
    [string] $Version = 'latest',
    [string] $CacheDir = $(if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA 'good-cat' }),
    [switch] $Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$stage = $null

function Assert-Tag([string] $Tag) {
    # Also exclude Windows device names and trailing dots (Win32 normalizes them).
    if ($Tag -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$' -or
        $Tag.EndsWith('.') -or
        $Tag -match '^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(?:\.|$)') {
        throw "Unsafe release tag: '$Tag'. Expected a single safe filename."
    }
}

function Assert-NoReparse([string] $Path) {
    $current = [IO.Path]::GetFullPath($Path)
    while ($current) {
        # GetAttributes also inspects reparse points instead of following their target.
        try {
            $attributes = [IO.File]::GetAttributes($current)
            if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Reparse points/symlinks are not allowed in cache paths: $current"
            }
        } catch [IO.FileNotFoundException] {
            # Nonexistent components are checked again after creation.
        } catch [IO.DirectoryNotFoundException] {
        }
        $parent = [IO.Path]::GetDirectoryName($current)
        if ($parent -eq $current) { break }
        $current = $parent
    }
}

function Get-CanonicalPath([string] $Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if ($full -notmatch '^[A-Za-z]:\\' -or $full.Substring(3) -match '[<>:"|?*\x00-\x1f]') {
        throw "Expected a local filesystem path without device syntax: $Path"
    }
    foreach ($component in $full.Substring(3).Split('\')) {
        if ($component -match '[ .]$' -or
            $component -match '^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(?:\.|$)') {
            throw "Unsafe Windows path component: $component"
        }
    }
    Assert-NoReparse $full
    # Expand existing 8.3 aliases, which otherwise bypass repository containment checks.
    $existing = $full
    $suffix = @()
    while (-not [IO.Directory]::Exists($existing) -and -not [IO.File]::Exists($existing)) {
        $suffix = @([IO.Path]::GetFileName($existing)) + $suffix
        $existing = [IO.Path]::GetDirectoryName($existing)
        if (-not $existing) { throw "Cannot resolve filesystem path: $Path" }
    }
    $buffer = New-Object Text.StringBuilder(32768)
    $length = [GoodCatBootstrap.Native]::GetLongPathName($existing, $buffer, $buffer.Capacity)
    if ($length -eq 0 -or $length -ge $buffer.Capacity) {
        throw "Cannot canonicalize filesystem path: $existing"
    }
    $canonical = $buffer.ToString()
    foreach ($component in $suffix) { $canonical = Join-Path $canonical $component }
    if ($canonical.Length -gt 3) { $canonical = $canonical.TrimEnd('\') }
    return $canonical
}

function Get-SourceDirectory([string] $Path) {
    # Skill managers may install symlinks/junctions. Resolve read-only source
    # paths with a directory handle; reparse points remain forbidden for caches.
    $full = [IO.Path]::GetFullPath($Path)
    $handle = [GoodCatBootstrap.Native]::CreateFile($full, 0, 7, [IntPtr]::Zero, 3, 0x02000000, [IntPtr]::Zero)
    if ($handle.IsInvalid) { $handle.Dispose(); throw "Cannot open skill source: $full" }
    try {
        $buffer = New-Object Text.StringBuilder(32768)
        $length = [GoodCatBootstrap.Native]::GetFinalPathNameByHandle($handle, $buffer, $buffer.Capacity, 0)
        if ($length -eq 0 -or $length -ge $buffer.Capacity) { throw 'Cannot resolve skill source path.' }
        $resolved = $buffer.ToString()
        if ($resolved.StartsWith('\\?\UNC\')) { $resolved = '\\' + $resolved.Substring(8) }
        elseif ($resolved.StartsWith('\\?\')) { $resolved = $resolved.Substring(4) }
        if ($resolved.Length -gt 3) { $resolved = $resolved.TrimEnd('\') }
        return $resolved
    } finally { $handle.Dispose() }
}

function Assert-CachePath([string] $Path) {
    $full = Get-CanonicalPath $Path
    foreach ($protected in @($script:skillRoot, $script:repositoryRoot)) {
        if ($protected -and ($full.Equals($protected, [StringComparison]::OrdinalIgnoreCase) -or
            $full.StartsWith($protected.TrimEnd('\') + [IO.Path]::DirectorySeparatorChar,
                [StringComparison]::OrdinalIgnoreCase))) {
            throw "Cache must be outside the repository and skill: $full"
        }
    }
    Assert-NoReparse $full
}

function Set-PrivateAccess([string] $Path, [bool] $Directory) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
    if ($Directory) {
        $acl = New-Object Security.AccessControl.DirectorySecurity
        $rule = New-Object Security.AccessControl.FileSystemAccessRule(
            $identity, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')
    } else {
        $acl = New-Object Security.AccessControl.FileSecurity
        $rule = New-Object Security.AccessControl.FileSystemAccessRule($identity, 'FullControl', 'Allow')
    }
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner($identity)
    $acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $acl
}

function New-CacheDirectory([string] $Path) {
    Assert-CachePath $Path
    if ([IO.File]::Exists($Path)) { throw "Cache directory path is a file: $Path" }
    $existed = [IO.Directory]::Exists($Path)
    [void] [IO.Directory]::CreateDirectory($Path)
    Assert-CachePath $Path
    if (-not $existed) { Set-PrivateAccess $Path $true }
}

function New-PrivateStage([string] $Path) {
    Assert-CachePath $Path
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetAccessRuleProtection($true, $false)
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $rule = New-Object Security.AccessControl.FileSystemAccessRule(
        $identity, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
    # Supply the ACL at creation, so downloaded files are never publicly readable.
    if ($PSVersionTable.PSEdition -eq 'Desktop') {
        [void] [IO.Directory]::CreateDirectory($Path, $acl)
    } else {
        $directory = New-Object IO.DirectoryInfo($Path)
        [IO.FileSystemAclExtensions]::Create($directory, $acl)
    }
    Assert-CachePath $Path
}

function Get-Https([Uri] $Uri, [string] $Destination = '') {
    # Handle redirects ourselves: automatic redirect behavior varies by .NET version,
    # and every hop, including a GitHub CDN redirect, must remain HTTPS.
    for ($hop = 0; $hop -le 10; $hop++) {
        if ($Uri.Scheme -cne 'https' -or $Uri.UserInfo) {
            throw "Refusing non-HTTPS or credential-bearing URL: $Uri"
        }
        $request = [Net.HttpWebRequest]::Create($Uri)
        $request.AllowAutoRedirect = $false
        $request.UserAgent = 'good-cat-windows-bootstrap'
        $request.Accept = 'application/vnd.github+json'
        $request.Timeout = 60000
        $request.ReadWriteTimeout = 60000
        $response = $null
        try {
            $response = $request.GetResponse()
            $code = [int] $response.StatusCode
            if ($code -in @(301, 302, 303, 307, 308)) {
                $location = $response.Headers['Location']
                if (-not $location) { throw 'Redirect without a Location header.' }
                $Uri = New-Object Uri($Uri, $location)
                continue
            }
            if ($code -ne 200) { throw "Unexpected HTTP status: $code" }
            $inputStream = $response.GetResponseStream()
            try {
                if ($Destination) {
                    $outputStream = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew,
                        [IO.FileAccess]::Write, [IO.FileShare]::None)
                    try { $inputStream.CopyTo($outputStream) } finally { $outputStream.Dispose() }
                } else {
                    $reader = New-Object IO.StreamReader($inputStream)
                    try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
                }
            } finally { $inputStream.Dispose() }
            return
        } finally {
            if ($null -ne $response) { $response.Dispose() }
        }
    }
    throw 'Too many HTTPS redirects.'
}

function Get-AssetUrl($Asset, [string] $Name, [string] $Tag) {
    $url = [Uri] ([string] $Asset.browser_download_url)
    $path = [Uri]::UnescapeDataString($url.AbsolutePath)
    $parts = $path.Split('/')
    if ($parts.Length -ne 7 -or ($parts[1] + '/' + $parts[2]) -ine $Repo) {
        throw "Asset is from a different repository: $Name"
    }
    # GitHub repository names are case-insensitive; tags and asset names are not.
    $expected = '/' + $parts[1] + '/' + $parts[2] + "/releases/download/$Tag/$Name"
    if (-not $url.IsAbsoluteUri -or $url.Scheme -cne 'https' -or
        $url.Host -ine 'github.com' -or $url.Port -ne 443 -or $url.UserInfo -or
        $url.Query -or $url.Fragment -or
        $path -cne $expected) {
        throw "Unexpected download URL for ${Name}: $url"
    }
    return $url
}

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

try {
    if ($Repo -cnotmatch '^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?/[A-Za-z0-9_.-]{1,100}$') {
        throw '-Repo must be a safe GitHub OWNER/REPO name.'
    }
    $owner, $repository = $Repo.Split('/')
    if ($owner.Contains('--') -or
        $owner -match '^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])$' -or
        $repository -in @('.', '..') -or
        $repository.EndsWith('.') -or
        $repository -match '^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(?:\.|$)') {
        throw '-Repo contains an unsafe owner or repository name.'
    }
    Assert-Tag $Version
    # Inspect the native OS, never PROCESSOR_ARCHITECTURE or the process bitness.
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'This bootstrap supports native Windows x64 only.'
    }
    # Native APIs also support PS5.1 without RuntimeInformation.OSArchitecture.
    # IsWow64Process2 avoids GetNativeSystemInfo's emulated-architecture reporting
    # on ARM64; older Windows versions without this API predate Windows ARM64.
    if (-not ('GoodCatBootstrap.Native' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
namespace GoodCatBootstrap {
    [StructLayout(LayoutKind.Explicit, Size = 64)]
    public struct SystemInfo {
        [FieldOffset(0)] public ushort ProcessorArchitecture;
    }
    public static class Native {
        [DllImport("kernel32.dll")]
        public static extern void GetNativeSystemInfo(out SystemInfo info);
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsWow64Process2(IntPtr process, out ushort processMachine, out ushort nativeMachine);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern uint GetLongPathName(string path, StringBuilder buffer, int size);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern Microsoft.Win32.SafeHandles.SafeFileHandle CreateFile(string path, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern uint GetFinalPathNameByHandle(Microsoft.Win32.SafeHandles.SafeFileHandle file, StringBuilder buffer, int size, uint flags);
    }
}
'@
    }
    [ushort] $processMachine = 0
    [ushort] $nativeMachine = 0
    try {
        if (-not [GoodCatBootstrap.Native]::IsWow64Process2([IntPtr](-1), [ref] $processMachine, [ref] $nativeMachine)) {
            throw 'Cannot determine native Windows OS architecture.'
        }
    } catch [EntryPointNotFoundException] {
        $systemInfo = New-Object GoodCatBootstrap.SystemInfo
        [GoodCatBootstrap.Native]::GetNativeSystemInfo([ref] $systemInfo)
        $nativeMachine = switch ($systemInfo.ProcessorArchitecture) {
            9 { 0x8664 }
            12 { 0xAA64 }
            default { 0 }
        }
    }
    if ($nativeMachine -ne 0x8664) {
        $description = if ($nativeMachine -eq 0xAA64) { 'ARM64' } else { "machine code $nativeMachine" }
        throw "Unsupported native Windows OS architecture: $description. Only x64 is supported; ARM64 is not supported."
    }
    if ([string]::IsNullOrWhiteSpace($CacheDir)) {
        throw 'LOCALAPPDATA is unavailable; provide -CacheDir outside the repository.'
    }
    # Reject alternate streams, reparse points, Win32 aliases and repository-local caches.
    if ($PSBoundParameters.ContainsKey('CacheDir')) {
        # The custom argument is a parent; never replace ACLs on that directory.
        $CacheDir = Join-Path $CacheDir 'good-cat'
    }
    $cacheRoot = Get-CanonicalPath $CacheDir
    $script:skillRoot = Get-SourceDirectory (Join-Path $PSScriptRoot '..')
    $candidate = Get-SourceDirectory (Join-Path $PSScriptRoot '..\..\..')
    $script:repositoryRoot = $null
    if ([IO.File]::Exists((Join-Path $candidate 'cli\Cargo.toml')) -and
        $script:skillRoot.Equals((Join-Path $candidate 'skill\good-cat'), [StringComparison]::OrdinalIgnoreCase)) {
        $script:repositoryRoot = $candidate
    }
    Assert-CachePath $cacheRoot
    $marker = Join-Path $cacheRoot '.good-cat-release-cache'
    if ([IO.Directory]::Exists($cacheRoot)) {
        if (-not [IO.File]::Exists($marker) -or [IO.File]::ReadAllText($marker).Trim() -cne 'good-cat-release-cache-v1') {
            throw 'Existing cache is not owned by good-cat; choose another cache parent.'
        }
        Assert-NoReparse $marker
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $acl = Get-Acl -LiteralPath $cacheRoot
        $ownerSid = $acl.GetOwner([Security.Principal.SecurityIdentifier])
        if ($ownerSid.Value -ne $identity.Value -or -not $acl.AreAccessRulesProtected) {
            throw 'Existing cache ownership or ACL is not private; choose another cache parent.'
        }
        foreach ($rule in $acl.Access) {
            if ($rule.AccessControlType -eq 'Allow' -and
                $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -ne $identity.Value) {
                throw 'Existing cache grants access to another identity; choose another cache parent.'
            }
        }
    }

    # PowerShell 5.1 may otherwise negotiate obsolete TLS defaults.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $endpoint = if ($Version -ceq 'latest') { 'latest' } else { 'tags/' + [Uri]::EscapeDataString($Version) }
    $release = (Get-Https ([Uri] "https://api.github.com/repos/$Repo/releases/$endpoint")) | ConvertFrom-Json
    $tag = [string] $release.tag_name
    Assert-Tag $tag
    if ($Version -cne 'latest' -and $tag -cne $Version) {
        throw "Release metadata tag '$tag' does not match requested tag '$Version'."
    }
    $target = 'x86_64-pc-windows-msvc'
    $assetName = "good-cat-$target.exe"
    $binaries = @($release.assets | Where-Object { $_.name -ceq $assetName })
    $manifests = @($release.assets | Where-Object { $_.name -ceq 'SHA256SUMS' })
    if ($binaries.Count -ne 1 -or $manifests.Count -ne 1) {
        throw "Release must contain exactly one $assetName and one SHA256SUMS asset."
    }
    $binaryUrl = Get-AssetUrl $binaries[0] $assetName $tag
    $manifestUrl = Get-AssetUrl $manifests[0] 'SHA256SUMS' $tag

    $directory = $cacheRoot
    New-CacheDirectory $directory
    if (-not [IO.File]::Exists($marker)) {
        [IO.File]::WriteAllText($marker, 'good-cat-release-cache-v1', [Text.Encoding]::ASCII)
    }
    foreach ($component in @($owner, $repository, $tag, $target)) {
        $directory = Join-Path $directory $component
        New-CacheDirectory $directory
    }
    $executable = Join-Path $directory $assetName
    Assert-CachePath $executable
    $stage = Join-Path $directory ('.stage-' + [Guid]::NewGuid().ToString('N'))
    New-PrivateStage $stage
    $manifestPath = Join-Path $stage 'SHA256SUMS'
    Get-Https $manifestUrl $manifestPath

    $hashes = @()
    foreach ($line in [IO.File]::ReadAllLines($manifestPath)) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        # GNU sha256sum text/binary forms; filenames must be exact basenames.
        if ($line -cnotmatch '^([0-9A-Fa-f]{64})[ \t]+\*?([^\r\n]+)$') {
            throw 'Malformed SHA256SUMS entry.'
        }
        $hash = $Matches[1].ToLowerInvariant()
        $name = $Matches[2]
        if ($name -ceq $assetName) { $hashes += $hash }
    }
    if ($hashes.Count -ne 1) {
        throw "SHA256SUMS must contain exactly one checksum for $assetName."
    }
    $expectedHash = $hashes[0]
    Assert-CachePath $executable
    if ([IO.Directory]::Exists($executable)) { throw "Executable cache path is a directory: $executable" }
    $reuse = -not $Force -and [IO.File]::Exists($executable) -and (Get-Sha256 $executable) -ceq $expectedHash
    if (-not $reuse) {
        $stagedExecutable = Join-Path $stage $assetName
        Get-Https $binaryUrl $stagedExecutable
        if ((Get-Sha256 $stagedExecutable) -cne $expectedHash) {
            throw "SHA256 mismatch for $assetName; existing cache has not been replaced."
        }
        Assert-CachePath $executable
        # Same-volume atomic replacement; never remove an old executable first.
        if ([IO.File]::Exists($executable)) {
            [IO.File]::Replace($stagedExecutable, $executable, [System.Management.Automation.Language.NullString]::Value)
        } else {
            [IO.File]::Move($stagedExecutable, $executable)
        }
    }
    Set-PrivateAccess $executable $false
    $result = [ordered] @{
        status = 'ok'
        repository = $Repo
        version = $tag
        target = $target
        executable = $executable
        sha256 = $expectedHash
    }
} catch {
    [Console]::Error.WriteLine('good-cat bootstrap: ' + $_.Exception.Message)
    exit 1
} finally {
    if ($stage -and [IO.Directory]::Exists($stage)) {
        try { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction Stop }
        catch { [Console]::Error.WriteLine('good-cat bootstrap: staging cleanup failed: ' + $_.Exception.Message) }
    }
}
# This is the only success output. All diagnostics go directly to stderr.
[Console]::Out.WriteLine(($result | ConvertTo-Json -Compress))
