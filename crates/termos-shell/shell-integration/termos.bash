# TermOS shell integration for bash (D-115, D-116).
#
# TermOS starts bash with `--rcfile` naming this file. It loads your own
# ~/.bashrc first, as bash would, then marks each prompt, command line,
# command output and exit status (OSC 133) so TermOS can tell commands
# apart. `[shell] integration = false` in TermOS's config turns it off.
if [ -r ~/.bashrc ]; then
    . ~/.bashrc
fi

if [[ $- == *i* && -z "${__termos_marks:-}" ]]; then
    __termos_marks=1

    # First of the prompt commands: the last command's exit status.
    __termos_status() {
        local s=$?
        printf '\e]133;D;%s\a' "$s"
        return $s
    }

    # Last of them: mark where the prompt starts and ends, again if a
    # prompt command rebuilt PS1.
    __termos_prompt() {
        local s=$?
        if [[ "$PS1" != *'133;A'* ]]; then
            PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
        fi
        return $s
    }

    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
        PROMPT_COMMAND=(__termos_status "${PROMPT_COMMAND[@]}" __termos_prompt)
    else
        PROMPT_COMMAND=$'__termos_status\n'"${PROMPT_COMMAND:-}"$'\n__termos_prompt'
    fi
    # bash 4.4 and later: the command's output starts.
    PS0="${PS0:-}"$'\e]133;C\a'
fi
