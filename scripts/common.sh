# Shared helpers for verify.sh and release-build.sh. Sourced, not executed.

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repository_root"

# CARGO_COMMAND lets a wrapper run cargo (for example as an unprivileged build account).
read -r -a cargo_command <<< "${CARGO_COMMAND:-cargo}"

# Bounded parallelism: one job per core, but no more than one per 1.5 GiB of available memory.
bounded_jobs() {
    local cores available_kib by_memory
    cores=$(nproc)
    available_kib=$(awk '/MemAvailable/ {print $2}' /proc/meminfo)
    by_memory=$(( available_kib / 1572864 ))
    (( by_memory < 1 )) && by_memory=1
    echo $(( cores < by_memory ? cores : by_memory ))
}

start_log_directory() {
    local kind=$1
    log_directory="$repository_root/artifacts/$kind/$(date -u +%Y%m%dT%H%M%SZ)"
    mkdir -p "$log_directory"
    summary="$log_directory/summary.txt"
    : > "$summary"
}

record_environment() {
    {
        echo "revision: $(git rev-parse HEAD)$(git diff --quiet HEAD -- || echo ' (dirty working tree)')"
        echo "host: $(uname -srm), $(nproc) cores, $(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo) MiB"
        echo "rustc: $("${cargo_command[@]:0:${#cargo_command[@]}-1}" rustc -vV 2>/dev/null | tr '\n' ' ' || true)"
        echo "cargo: $("${cargo_command[@]}" --version 2>&1)"
        echo "jobs: $CARGO_BUILD_JOBS"
        grep -v '^#' scripts/upstream-revisions.env | sed 's/^/upstream: /'
    } >> "$summary"
}

failures=0

# run_step NAME COMMAND...: logs to NAME.log, records PASS/FAIL with the exact command.
run_step() {
    local name=$1; shift
    local started=$SECONDS
    echo "==> $name: $*"
    if "$@" > "$log_directory/$name.log" 2>&1; then
        echo "PASS $name ($(( SECONDS - started )) s): $*" >> "$summary"
    else
        echo "FAIL $name ($(( SECONDS - started )) s): $*" >> "$summary"
        tail -n 30 "$log_directory/$name.log"
        failures=$(( failures + 1 ))
    fi
}

skip_step() {
    echo "NOT RUN $1: $2" >> "$summary"
    echo "==> $1 not run: $2"
}

finish() {
    cat "$summary"
    echo "logs: $log_directory"
    if (( failures > 0 )); then
        echo "$failures step(s) failed" >&2
        exit 1
    fi
}
