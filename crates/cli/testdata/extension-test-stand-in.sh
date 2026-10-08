#!/bin/sh
case "$2" in
    *pass.json) printf 'ok pass\n'; exit 0 ;;
    *) printf 'FAIL fail\nstand-in reason\n'; exit 1 ;;
esac
