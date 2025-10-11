#compdef iotop

_iotop() {
    local -a args

    args=(
        '(-o --only)'{-o,--only}'[only show processes or threads actually doing I/O]'
        '(-P --processes)'{-P,--processes}'[show processes, not all threads]'
        '(-a --accumulated)'{-a,--accumulated}'[show accumulated I/O instead of bandwidth]'
        '(-d --delay)'{-d,--delay}'[delay between iterations in seconds]:delay (seconds):(0.5 1 2 5 10)'
        '(-n --iter)'{-n,--iter}'[number of iterations before ending]:iterations:(5 10 20 50 100)'
        '(-b --batch)'{-b,--batch}'[batch mode (non-interactive)]'
        '*'{-p,--pid}'[processes/threads to monitor]:pid:_pids'
        '*'{-u,--user}'[users to monitor]:user:_users'
        '(-t --time)'{-t,--time}'[add timestamp on each line (implies --batch)]'
        '(-q --quiet)'{-q,--quiet}'[suppress column names and headers (implies --batch)]'
        '(-k --kilobytes)'{-k,--kilobytes}'[use kilobytes instead of human-friendly units]'
        '(-h --help)'{-h,--help}'[show help information]'
    )

    _arguments -s -S $args
}

_iotop "$@"
