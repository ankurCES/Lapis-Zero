#!/usr/bin/env bash
# Install the Lapis-Zero Android companion (debug build) on the attached phone and launch it.
#
#   scripts/deploy_android.sh              install the debug APK (building it first if missing)
#   scripts/deploy_android.sh --build      always rebuild before installing
#   scripts/deploy_android.sh --reverse    also forward the phone's 127.0.0.1:8421 to this
#                                          computer (for "Dev connect" to a local mock daemon)
#   scripts/deploy_android.sh --reverse 18431
#
# Exactly one authorized device must be attached, or ANDROID_SERIAL must name the one to use.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ANDROID_DIR="$(cd "${SCRIPT_DIR}/../android" && pwd)"
APK="${ANDROID_DIR}/app/build/outputs/apk/debug/app-debug.apk"
PACKAGE="dev.lapiszero.app"
ACTIVITY="${PACKAGE}/.MainActivity"
DEFAULT_PORT=8421

die() {
    printf 'deploy_android: %s\n' "$*" >&2
    exit 1
}

usage() {
    sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

force_build=0
reverse_port=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --build)
            force_build=1
            ;;
        --reverse)
            reverse_port="${DEFAULT_PORT}"
            if [ "$#" -gt 1 ] && [[ "$2" =~ ^[0-9]+$ ]]; then
                reverse_port="$2"
                shift
            fi
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
    shift
done

# --- adb ---------------------------------------------------------------------

find_adb() {
    local candidate
    if command -v adb >/dev/null 2>&1; then
        command -v adb
        return 0
    fi
    for candidate in \
        "${ANDROID_HOME:-}/platform-tools/adb" \
        "${ANDROID_SDK_ROOT:-}/platform-tools/adb" \
        "${HOME}/Library/Android/sdk/platform-tools/adb" \
        "${HOME}/Android/Sdk/platform-tools/adb"; do
        if [ -x "${candidate}" ]; then
            printf '%s\n' "${candidate}"
            return 0
        fi
    done
    return 1
}

ADB="$(find_adb)" || die "adb not found (install Android platform-tools or set ANDROID_HOME)"

# Prints "serial<TAB>state" for every attached device.
list_devices() {
    "${ADB}" devices | awk 'NR > 1 && NF >= 2 { print $1 "\t" $2 }'
}

select_device() {
    local devices serial state authorized count
    devices="$(list_devices)"

    if [ -n "${ANDROID_SERIAL:-}" ]; then
        state="$(printf '%s\n' "${devices}" | awk -F '\t' -v s="${ANDROID_SERIAL}" '$1 == s { print $2 }')"
        case "${state}" in
            device) ;;
            "") die "ANDROID_SERIAL=${ANDROID_SERIAL} is not attached (see: adb devices)" ;;
            unauthorized) die "${ANDROID_SERIAL} is unauthorized: accept the USB debugging prompt on the phone" ;;
            *) die "${ANDROID_SERIAL} is in state '${state}', not ready for install" ;;
        esac
        printf '%s\n' "${ANDROID_SERIAL}"
        return 0
    fi

    authorized="$(printf '%s\n' "${devices}" | awk -F '\t' '$2 == "device" { print $1 }')"
    count="$(printf '%s' "${authorized}" | grep -c . || true)"
    case "${count}" in
        1)
            printf '%s\n' "${authorized}"
            ;;
        0)
            if [ -n "${devices}" ]; then
                while IFS="$(printf '\t')" read -r serial state; do
                    printf 'deploy_android:   %s is %s\n' "${serial}" "${state}" >&2
                done <<<"${devices}"
                die "no authorized device: unlock the phone and accept the USB debugging prompt"
            fi
            die "no device attached: connect a phone with USB debugging enabled"
            ;;
        *)
            printf 'deploy_android: %s authorized devices attached:\n' "${count}" >&2
            printf '%s\n' "${authorized}" | sed 's/^/deploy_android:   /' >&2
            die "set ANDROID_SERIAL to choose one"
            ;;
    esac
}

SERIAL="$(select_device)"

# --- build (only when needed) --------------------------------------------------

# Gradle needs a JDK 17+; nothing exports one by default on a fresh macOS/Homebrew setup.
ensure_java() {
    local candidate
    if [ -n "${JAVA_HOME:-}" ] && [ -x "${JAVA_HOME}/bin/java" ]; then
        return 0
    fi
    for candidate in \
        /opt/homebrew/opt/openjdk@21 \
        /opt/homebrew/opt/openjdk@17 \
        /usr/local/opt/openjdk@21 \
        /usr/local/opt/openjdk@17; do
        if [ -x "${candidate}/bin/java" ]; then
            export JAVA_HOME="${candidate}"
            return 0
        fi
    done
    if [ -x /usr/libexec/java_home ] && candidate="$(/usr/libexec/java_home -v 17+ 2>/dev/null)"; then
        export JAVA_HOME="${candidate}"
        return 0
    fi
    if command -v java >/dev/null 2>&1 && java -version >/dev/null 2>&1; then
        return 0
    fi
    die "no JDK found: set JAVA_HOME to a JDK 17 or newer"
}

if [ "${force_build}" -eq 1 ] || [ ! -f "${APK}" ]; then
    ensure_java
    printf 'deploy_android: building the debug APK...\n'
    (cd "${ANDROID_DIR}" && ./gradlew --console=plain assembleDebug)
fi
[ -f "${APK}" ] || die "build finished but ${APK} is missing"

# --- install and launch --------------------------------------------------------

printf 'deploy_android: installing %s on %s\n' "$(basename "${APK}")" "${SERIAL}"
"${ADB}" -s "${SERIAL}" install -r "${APK}"

if [ -n "${reverse_port}" ]; then
    printf 'deploy_android: adb reverse tcp:%s -> this computer\n' "${reverse_port}"
    "${ADB}" -s "${SERIAL}" reverse "tcp:${reverse_port}" "tcp:${reverse_port}"
fi

printf 'deploy_android: launching %s\n' "${ACTIVITY}"
"${ADB}" -s "${SERIAL}" shell am start -n "${ACTIVITY}"
