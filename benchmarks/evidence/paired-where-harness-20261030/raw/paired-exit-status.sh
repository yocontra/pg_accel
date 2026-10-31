#!/usr/bin/env bash
# Pure status precedence; sourced by the measurement cleanup and offline cases.
paired_exit_status() {
 if test "$1" -ne 0; then printf '%s\n' "$1"
 elif test "$2" -ne 0; then printf '1\n'
 else printf '0\n'
 fi
}
