# TermOS shell integration for PowerShell (D-115, D-116).
#
# TermOS starts PowerShell with `-NoExit -Command` running this file after
# your profile. It marks each prompt, command line, command output and
# exit status (OSC 133) so TermOS can tell commands apart.
# `[shell] integration = false` in TermOS's config turns it off.
if ($global:__TermosMarks) { return }
$global:__TermosMarks = $true
$global:__TermosPrompt = $function:prompt
$global:__TermosHistory = (Get-History -Count 1).Id

# PSReadLine reads each command line: report it, then its output starts.
function global:__TermosWrapReadLine {
    if ($global:__TermosReadLine) { return }
    if (-not (Get-Command PSConsoleHostReadLine -ErrorAction SilentlyContinue)) { return }
    $global:__TermosReadLine = $function:PSConsoleHostReadLine
    function global:PSConsoleHostReadLine {
        $line = & $global:__TermosReadLine
        if ($line -and $line.Trim()) {
            $esc = [char]27
            $bel = [char]7
            $e = $line.Replace('\', '\\').Replace(';', '\x3b').Replace("`n", '\x0a').Replace("`r", '\x0d')
            [Console]::Write("$esc]633;E;$e$bel$esc]133;C$bel")
        }
        $line
    }
}

function global:prompt {
    $ok = $?
    $code = $global:LASTEXITCODE
    $esc = [char]27
    $bel = [char]7
    $out = ''
    # A command ran since the last prompt: its exit status.
    $last = (Get-History -Count 1).Id
    if ($last -and $last -ne $global:__TermosHistory) {
        $global:__TermosHistory = $last
        $s = if ($ok) { 0 } elseif ($code) { $code } else { 1 }
        $out += "$esc]133;D;$s$bel"
    }
    # PSReadLine may load after this file ran.
    __TermosWrapReadLine
    $text = & $global:__TermosPrompt
    $global:LASTEXITCODE = $code
    return "$out$esc]133;A$bel$text$esc]133;B$bel"
}

__TermosWrapReadLine
