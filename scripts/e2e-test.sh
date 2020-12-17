#!/usr/bin/env bash

set -eux

SCRIPTDIR=$(dirname "$(realpath "$0")")
# new tests should be added before the replica_pod_remove test
#TESTS="install basic_volume_io csi replica rebuild node_disconnect/replica_pod_remove uninstall"
TESTS="install basic_volume_io csi uninstall"
EXTENDED_TESTS=""
TESTDIR=$(realpath "$SCRIPTDIR/../test/e2e")
REPORTSDIR=$(realpath "$SCRIPTDIR/..")

# Global state variables
tests=""
run_extended_tests=
device=
registry=
tag=
generate_logs=0
on_fail="continue"

help() {
  cat <<EOF
Usage: $0 [OPTIONS]

Options:
  --device <path>           Device path to use for storage pools.
  --registry <host[:port]>  Registry to pull the mayastor images from.
  --tag <name>              Docker image tag of mayastor images (default "ci")
  --tests <list of tests>   Lists of tests to run, delimited by spaces (default: "$TESTS")
        Note: the last 2 tests should be (if they are to be run)
             node_disconnect/replica_pod_remove uninstall
  --extended                Run long running tests also.
  --reportsdir <path>       Path to use for junit xml test reports (default: repo root)
  --logs                    Generate logs and cluster state dump at the end of successful test run.
  --onfail <stop|continue>  On fail, stop immediately or continue default($on_fail)
                            Behaviour for "continue" only differs if uninstall is in the list of tests (the default).

Examples:
  $0 --registry 127.0.0.1:5000 --tag a80ce0c
EOF
}

# Parse arguments
while [ "$#" -gt 0 ]; do
  case "$1" in
    -d|--device)
      shift
      device=$1
      ;;
    -r|--registry)
      shift
      registry=$1
      ;;
    -t|--tag)
      shift
      tag=$1
      ;;
    -T|--tests)
      shift
      tests="$1"
      ;;
    -R|--reportsdir)
      shift
      REPORTSDIR="$1"
      ;;
    -h|--help)
      help
      exit 0
      ;;
    -l|--logs)
      generate_logs=1
      ;;
    -e|--extended)
      run_extended_tests=1
      ;;
    --onfail)
        shift
        case $1 in
            continue)
                on_fail=$1
                ;;
            stop)
                on_fail=$1
                ;;
            *)
                help
                exit 2
        esac
        ;;
    *)
      echo "Unknown option: $1"
      help
      exit 1
      ;;
  esac
  shift
done

if [ -z "$device" ]; then
  echo "Device for storage pools must be specified"
  help
  exit 1
fi
export e2e_pool_device=$device

if [ -n "$tag" ]; then
  export e2e_image_tag="$tag"
fi

if [ -n "$registry" ]; then
  export e2e_docker_registry="$registry"
fi

if [ -z "$tests" ]; then
  tests="$TESTS"
  if [ -n "$run_extended_tests" ]; then
    tests="$tests $EXTENDED_TESTS"
  fi
fi

export e2e_reports_dir="$REPORTSDIR"
if [ ! -d "$e2e_reports_dir" ] ; then
    echo "Reports directory $e2e_reports_dir does not exist"
    exit 1
fi

test_failed=0

# Run go test in directory specified as $1 (relative path)
function runGoTest {
    cd "$TESTDIR"
    echo "Running go test in $PWD/\"$1\""
    if [ -z "$1" ] || [ ! -d "$1" ]; then
        return 1
    fi

    cd "$1"
    if ! go test -v . -ginkgo.v -ginkgo.progress -timeout 0; then
        return 1
    fi

    return 0
}

# Check if $2 is in $1
contains() {
    [[ $1 =~ (^|[[:space:]])$2($|[[:space:]]) ]] && return 0  || return 1
}

echo "list of tests: $tests"
for dir in $tests; do
  # defer uninstall till after other tests have been run.
  if [ "$dir" != "uninstall" ] ;  then
      if ! runGoTest "$dir" ; then
          test_failed=1
          break
      fi

      if ! ("$SCRIPTDIR"/e2e_check_pod_restarts.sh) ; then
          test_failed=1
          break
      fi

  fi
done

if [ "$test_failed" -ne 0 ]; then
    if ! "$SCRIPTDIR"/e2e-cluster-dump.sh ; then
        # ignore failures in the dump script
        :
    fi

    if [ "$on_fail" == "stop" ]; then
        exit 3
    fi
fi

# Always run uninstall test if specified
if contains "$tests" "uninstall" ; then
    if ! runGoTest "uninstall" ; then
        test_failed=1
        if ! "$SCRIPTDIR"/e2e-cluster-dump.sh --clusteronly ; then
            # ignore failures in the dump script
            :
        fi
    fi
fi

if [ "$test_failed" -ne 0 ]; then
    echo "At least one test has FAILED!"
  exit 1
fi

if [ "$generate_logs" -ne 0 ]; then
    if ! "$SCRIPTDIR"/e2e-cluster-dump.sh ; then
        # ignore failures in the dump script
        :
    fi
fi

echo "All tests have PASSED!"
exit 0
