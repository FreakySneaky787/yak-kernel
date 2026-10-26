# Soak test: `sh /soak.sh [rounds]` repeats a mix of file, pipe, process,
# thread, signal, floating point, network and syscall work. `make soak`
# (scripts/soak.py) runs it and also watches the kernel for panics.
rounds=$1
[ -n "$rounds" ] || rounds=10
fail=0
round=0
[ -d /soak ] || mkdir /soak

bad() {
    echo "soak: FAIL in round $round: $1"
    cat /soak/log
    fail=$((fail + 1))
}

while [ $round -lt $rounds ]; do
    round=$((round + 1))

    # files: create, append, copy, rename, read back, remove
    echo "round $round" > /soak/a.txt
    cat /README.txt >> /soak/a.txt
    cp /soak/a.txt /soak/b.txt
    mv /soak/b.txt /soak/c.txt
    grep "round $round" /soak/c.txt > /soak/log || bad "file contents"
    rm /soak/a.txt
    rm /soak/c.txt
    [ -f /soak/c.txt ] && bad "rm"

    # pipes through several processes
    cat /README.txt /README.txt | sort | grep -v nothing | wc > /soak/log || bad "pipeline"
    grep " 8 " /soak/log > /soak/log2 || bad "pipeline line count"

    # self-tests: processes, pipes, heap, threads, signals, FPU, file syscalls
    spawntest > /soak/log 2>&1 || bad spawntest
    pipetest > /soak/log 2>&1 || bad pipetest
    heaptest > /soak/log 2>&1 || bad heaptest
    threadtest > /soak/log 2>&1 || bad threadtest
    sigtest > /soak/log 2>&1 || bad sigtest
    fstest > /soak/log 2>&1 || bad fstest

    # background jobs running at the same time
    fputest > /soak/fpu.log 2>&1 &
    syscheck 2000 $round > /soak/sys.log 2>&1 &
    wait
    grep "all checks passed" /soak/fpu.log > /soak/log || bad fputest
    grep "all checks passed" /soak/sys.log > /soak/log || bad syscheck

    # network: the gateway, and HTTP over loopback to our own httpd
    ping 10.0.2.2 1 > /soak/log 2>&1 || bad ping
    httpd 8080 -n 1 > /soak/httpd.log 2>&1 &
    wget -O /soak/got.txt http://10.0.2.15:8080/README.txt > /soak/log 2>&1 || bad wget
    wait
    grep "YAK disk" /soak/got.txt > /soak/log || bad "wget contents"

    echo "soak: round $round of $rounds done, $fail failures"
done
rm /soak/log
rm /soak/log2
rm /soak/fpu.log
rm /soak/sys.log
rm /soak/got.txt
rm /soak/httpd.log
echo "soak: finished $rounds rounds with $fail failures"
exit $fail
