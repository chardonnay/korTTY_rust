#!/bin/bash
# Run a build or smoke check with a private, disposable workspace directory.
set -euo pipefail

scratch_dir=
child_pid=

# Stop the process group started by this script before removing its files.
cleanup() {
  local status="$?"
  local attempt
  trap - EXIT
  trap '' HUP INT TERM

  if [[ -n "${child_pid}" ]]; then
    if kill -TERM -- "-${child_pid}" 2>/dev/null; then
      for ((attempt = 0; attempt < 30; attempt++)); do
        kill -0 -- "-${child_pid}" 2>/dev/null || break
        sleep 0.1
      done
      kill -KILL -- "-${child_pid}" 2>/dev/null || true
    fi
    wait "${child_pid}" 2>/dev/null || true
  fi

  if [[ -n "${scratch_dir}" ]] && ! rm -rf -- "${scratch_dir}"; then
    printf 'Could not remove scratch directory %s\n' "${scratch_dir}" >&2
    if [[ "${status}" == 0 ]]; then
      status=1
    fi
  fi
  exit "${status}"
}

# Keep the caller's working directory and isolate the command's temp files.
main() {
  local workspace
  local zsh_tests=false
  if [[ "${1:-}" == --zsh-tests ]]; then
    zsh_tests=true
    shift
  fi
  if (( $# == 0 )); then
    printf 'Usage: %s [--zsh-tests] command [argument ...]\n' "$0" >&2
    return 2
  fi

  workspace="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
  mkdir -p -- "${workspace}/target/.scratch"
  trap cleanup EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  scratch_dir="$(mktemp -d "${workspace}/target/.scratch/run.XXXXXXXX")"
  export TMPDIR="${scratch_dir}"
  export TMP="${scratch_dir}"
  export TEMP="${scratch_dir}"

  if [[ "${zsh_tests}" == true ]]; then
    if [[ ! -x /bin/zsh ]]; then
      printf 'zsh tests require /bin/zsh\n' >&2
      return 1
    fi
    export SHELL=/bin/zsh
    export ZDOTDIR="${scratch_dir}/zsh"
    mkdir -m 700 -- "${ZDOTDIR}"
    # macOS zsh otherwise shares one history lock across concurrent test PTYs.
    printf '%s\n' 'unset HISTFILE' 'SAVEHIST=0' > "${ZDOTDIR}/.zshrc"
  fi

  # Bash job control creates a separate process group for the owned command.
  set -m
  "$@" </dev/null &
  child_pid="$!"
  wait "${child_pid}"
}

main "$@"
