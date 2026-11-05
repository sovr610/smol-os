# TermOS shell integration for zsh (D-115, D-116).
#
# TermOS starts zsh with ZDOTDIR pointing here. This puts your own
# ZDOTDIR back and loads your .zshenv; zsh then reads your .zprofile,
# .zshrc and .zlogin as usual. In an interactive shell it also marks each
# prompt, command line, command output and exit status (OSC 133) so TermOS
# can tell commands apart. `[shell] integration = false` in TermOS's
# config turns it off.
if [[ -n "${TERMOS_ZDOTDIR+x}" ]]; then
    ZDOTDIR=$TERMOS_ZDOTDIR
    unset TERMOS_ZDOTDIR
else
    unset ZDOTDIR
fi
if [[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]]; then
    source "${ZDOTDIR:-$HOME}/.zshenv"
fi

if [[ -o interactive && -z "${__termos_marks:-}" ]]; then
    typeset -g __termos_marks=1
    typeset -g __termos_ran=

    # First at each prompt: the last command's status, then the prompt.
    __termos_precmd() {
        local s=$?
        if [[ -n $__termos_ran ]]; then
            print -n "\e]133;D;$s\a"
            __termos_ran=
        fi
        print -n '\e]133;A\a'
        return $s
    }

    # When the line editor starts: the command line starts.
    __termos_line_init() {
        print -n '\e]133;B\a'
    }

    # Before a command runs: its line (VS Code's escaping), then its
    # output starts.
    __termos_preexec() {
        local line=$1
        line=${line//\\/\\\\}
        line=${line//;/\\x3b}
        line=${line//$'\n'/\\x0a}
        print -rn -- $'\e]633;E;'"$line"$'\a\e]133;C\a'
        __termos_ran=1
    }

    precmd_functions=(__termos_precmd $precmd_functions)
    preexec_functions+=(__termos_preexec)
    autoload -Uz add-zle-hook-widget 2>/dev/null &&
        add-zle-hook-widget line-init __termos_line_init 2>/dev/null
fi
