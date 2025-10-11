# fish completion for iotop

# Options
complete -c iotop -s o -l only -d 'Only show processes or threads actually doing I/O'
complete -c iotop -s P -l processes -d 'Show processes, not all threads'
complete -c iotop -s a -l accumulated -d 'Show accumulated I/O instead of bandwidth'
complete -c iotop -s d -l delay -d 'Delay between iterations in seconds' -x -a '0.5 1 2 5 10'
complete -c iotop -s n -l iter -d 'Number of iterations before ending' -x -a '5 10 20 50 100'
complete -c iotop -s b -l batch -d 'Batch mode (non-interactive)'
complete -c iotop -s p -l pid -d 'Processes/threads to monitor' -x -a '(__fish_complete_pids)'
complete -c iotop -s u -l user -d 'Users to monitor' -x -a '(__fish_complete_users)'
complete -c iotop -s t -l time -d 'Add timestamp on each line (implies --batch)'
complete -c iotop -s q -l quiet -d 'Suppress column names and headers (implies --batch)'
complete -c iotop -s k -l kilobytes -d 'Use kilobytes instead of human-friendly units'
complete -c iotop -s h -l help -d 'Show help information'
