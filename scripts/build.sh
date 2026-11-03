#!/bin/sh
# Builds Podėlis with the database engines you choose. It asks with
# checkboxes (whiptail or dialog, where there is one; else a numbered
# list), then runs cargo with those engines' features. The engines are
# read from their folders (crates/podelis-core/src/db/*/engine.toml),
# so a new one shows here by itself; opt-in ones start unticked.
#
#   scripts/build.sh                    ask, then install (cargo install)
#   scripts/build.sh --build            ask, then build (target/release)
#   scripts/build.sh --engines a,b      no questions: these features
#   scripts/build.sh --print            say the cargo command, don't run it
#
# Without a terminal (CI, a pipe) it asks nothing: --engines, else the
# last choice, else the defaults. The choice is kept in .podelis-engines
# as the next run's ticks.
set -eu
cd "$(dirname "$0")/.."

action=install engines="" print=0
while [ $# -gt 0 ]; do
    case "$1" in
        --build) action=build ;;
        --print) print=1 ;;
        --engines) engines=${2:?--engines takes a list, e.g. postgres,sqlite}; shift ;;
        --engines=*) engines=${1#--engines=} ;;
        -h | --help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
    shift
done

# `key = "value"` from an engine.toml (`#` starts a comment).
field() {
    sed -n "s/^$2[[:space:]]*=[[:space:]]*\"\{0,1\}\([^\"#]*\)\"\{0,1\}.*/\1/p" "$1" | head -n 1 | sed 's/[[:space:]]*$//'
}

# One line per engine, in their order: order|feature|label|opt_in|note.
list=$(for toml in crates/podelis-core/src/db/*/engine.toml; do
    dir=$(basename "$(dirname "$toml")")
    order=$(field "$toml" order)
    feature=$(field "$toml" feature)
    label=$(field "$toml" label)
    printf '%s|%s|%s|%s|%s\n' "${order:-99}" "${feature:-$dir}" "${label:-$dir}" \
        "$(field "$toml" opt_in)" "$(field "$toml" note)"
done | sort -t '|' -k1,1n)

# Ticked to begin with: the last choice, else every engine not opt-in.
if [ -f .podelis-engines ]; then
    ticked=$(cat .podelis-engines)
else
    ticked=$(printf '%s\n' "$list" | awk -F '|' '$4 != "true" { printf "%s%s", sep, $2; sep = "," }')
fi
on() { case ",$ticked," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

ask_whiptail() {
    tool=$1
    set --
    while IFS='|' read -r _ feature label _ note; do
        state=OFF
        on "$feature" && state=ON
        set -- "$@" "$feature" "$label${note:+ — $note}" "$state"
    done <<EOF
$list
EOF
    # The choice comes back on stderr: swapped with stdout to read it.
    choice=$("$tool" --title "Podėlis" --checklist \
        "Which databases should Podėlis talk to? Space ticks, Enter builds." \
        20 76 8 "$@" 3>&1 1>&2 2>&3) || { echo "build.sh: nothing built" >&2; exit 1; }
    printf '%s' "$choice" | tr -d '"' | tr ' ' ','
}

ask_numbers() {
    echo "Which databases should Podėlis talk to?" >&2
    n=0
    while IFS='|' read -r _ feature label _ note; do
        n=$((n + 1))
        mark=" "
        on "$feature" && mark=x
        printf '  %d [%s] %s%s\n' "$n" "$mark" "$label" "${note:+ ($note)}" >&2
    done <<EOF
$list
EOF
    printf 'Numbers to build with (Enter keeps those ticked): ' >&2
    read -r answer || answer=""
    if [ -z "$answer" ]; then
        printf '%s' "$ticked"
        return
    fi
    for k in $answer; do
        printf '%s\n' "$list" | sed -n "${k}p" | cut -d '|' -f 2
    done | paste -s -d ',' -
}

if [ -z "$engines" ]; then
    if [ -t 0 ] && [ -t 1 ]; then
        if command -v whiptail > /dev/null; then
            engines=$(ask_whiptail whiptail)
        elif command -v dialog > /dev/null; then
            engines=$(ask_whiptail dialog)
        else
            engines=$(ask_numbers)
        fi
    else
        engines=$ticked
    fi
fi
if [ -z "$engines" ]; then
    echo "build.sh: no engine chosen: Podėlis needs at least one" >&2
    exit 1
fi
# Every one asked for is an engine here.
for e in $(printf '%s' "$engines" | tr ',' ' '); do
    printf '%s\n' "$list" | cut -d '|' -f 2 | grep -qx "$e" || {
        echo "build.sh: no engine '$e' (there are: $(printf '%s\n' "$list" | cut -d '|' -f 2 | paste -s -d ' ' -))" >&2
        exit 2
    }
done

if [ "$action" = install ]; then
    set -- cargo install --locked --path . --no-default-features --features "$engines"
else
    set -- cargo build --locked --release --no-default-features --features "$engines"
fi
echo "==> $*"
[ "$print" = 1 ] && exit 0
printf '%s\n' "$engines" > .podelis-engines
exec "$@"
