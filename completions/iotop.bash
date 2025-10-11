# bash completion for iotop

_iotop() {
    local cur prev opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    opts="-o --only -P --processes -a --accumulated -d --delay -n --iter -b --batch -p --pid -u --user -t --time -q --quiet -k --kilobytes -h --help"

    case "${prev}" in
        -d|--delay)
            # Suggest some common delay values
            COMPREPLY=( $(compgen -W "0.5 1 2 5 10" -- ${cur}) )
            return 0
            ;;
        -n|--iter)
            # Suggest some common iteration counts
            COMPREPLY=( $(compgen -W "5 10 20 50 100" -- ${cur}) )
            return 0
            ;;
        -p|--pid)
            # Complete with PIDs
            COMPREPLY=( $(compgen -W "$(ps -e -o pid= | tr '\n' ' ')" -- ${cur}) )
            return 0
            ;;
        -u|--user)
            # Complete with usernames
            COMPREPLY=( $(compgen -u -- ${cur}) )
            return 0
            ;;
        *)
            ;;
    esac

    if [[ ${cur} == -* ]] ; then
        COMPREPLY=( $(compgen -W "${opts}" -- ${cur}) )
        return 0
    fi
}

complete -F _iotop iotop
