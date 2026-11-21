#!/usr/bin/env bash
# Repo tidiness invariants: thin Makefile, well-formed scripts, README documents targets.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
err() { echo "tidy: $*" >&2; fail=1; }

targets=()
while IFS= read -r line; do
  [[ -z "$line" || "$line" == \#* || "$line" == .PHONY:* ]] && continue
  if [[ "$line" =~ ^([a-z][a-z0-9-]*):[[:space:]]*$ ]]; then
    targets+=("${BASH_REMATCH[1]}")
    current="${BASH_REMATCH[1]}"
    continue
  fi
  if [[ "$line" == $'\t'* ]]; then
    recipe="${line#$'\t'}"
    [[ "$recipe" =~ ^@\./scripts/([a-z0-9-]+)\.sh$ ]] || { err "recipe for '${current:-?}' is not a single @./scripts/<x>.sh line: $recipe"; continue; }
    script="scripts/${BASH_REMATCH[1]}.sh"
    [[ -f "$script" ]] || err "$script referenced by Makefile does not exist"
    continue
  fi
  err "unexpected Makefile line: $line"
done < Makefile

for s in scripts/*.sh; do
  [[ -x "$s" ]] || err "$s is not executable"
  [[ "$(sed -n 1p "$s")" == "#!/usr/bin/env bash" ]] || err "$s must start with #!/usr/bin/env bash"
  grep -q '^set -euo pipefail$' "$s" || err "$s must contain 'set -euo pipefail'"
done

for t in "${targets[@]}"; do
  grep -q "make ${t}" README.md || err "README.md does not document 'make ${t}'"
done

exit "$fail"
