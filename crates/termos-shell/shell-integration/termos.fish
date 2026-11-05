# TermOS shell integration for fish (D-115, D-116).
#
# TermOS starts fish with this file's folder first in XDG_DATA_DIRS, so
# fish loads it with its vendor configuration, before your config.fish.
# It puts XDG_DATA_DIRS back for the programs fish starts. fish 4 marks
# prompts, commands and exit status (OSC 133) itself; for older fish this
# does. Either way it reports each command line, so TermOS can tell
# commands apart. `[shell] integration = false` in TermOS's config turns
# it off.
if set -q TERMOS_XDG_DATA_DIRS
    set -gx XDG_DATA_DIRS $TERMOS_XDG_DATA_DIRS
    set -e TERMOS_XDG_DATA_DIRS
else
    set -e XDG_DATA_DIRS
end

status is-interactive; or exit
set -q __termos_marks; and exit
set -g __termos_marks 1

function __termos_preexec --on-event fish_preexec
    set -l line (string replace -a -- '\\' '\\\\' $argv[1] | string replace -a -- ';' '\\x3b' | string join '\\x0a')
    printf '\e]633;E;%s\a' "$line"
end

if test (string split . -- $version)[1] -lt 4
    function __termos_prompt --on-event fish_prompt
        printf '\e]133;A\a'
    end
    function __termos_output --on-event fish_preexec
        printf '\e]133;C\a'
    end
    function __termos_status --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
end
