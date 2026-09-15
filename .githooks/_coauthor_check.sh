# - Shared logic for the commit-msg and pre-push hooks.
# - Source it with the dot operator, not by running it.
# - _coauthor_check takes one argument, the allowlist path.
# - It reads message lines from standard input.
# - It prints each blocked co-author line to standard error.
# - It returns one when it blocks a line, else zero.
# - _cc_key reads one line and prints its identity key.
# - The key is the email, lowercased, with no spaces.
# - With no email, the key is the whole value.

_cc_key() {
  _cc_line=$(cat)
  _cc_email=$(printf '%s' "$_cc_line" | sed -n 's/.*<\([^>]*\)>.*/\1/p')
  if [ -n "$_cc_email" ]
  then
    printf '%s' "$_cc_email" | tr 'A-Z' 'a-z' | tr -d '[:space:]'
  else
    printf '%s' "$_cc_line" | tr 'A-Z' 'a-z' | tr -d '[:space:]'
  fi
}

_coauthor_check() {
  _cc_allow=$1
  _cc_allowed=" "
  if [ -f "$_cc_allow" ]
  then
    while IFS= read -r _cc_l
    do
      _cc_l=$(printf '%s' "$_cc_l" | sed -e 's/^[[:space:]]*//' -e 's/#.*//')
      [ -n "$_cc_l" ] || continue
      _cc_k=$(printf '%s\n' "$_cc_l" | _cc_key)
      [ -n "$_cc_k" ] || continue
      _cc_allowed="$_cc_allowed$_cc_k "
    done < "$_cc_allow"
  fi
  _cc_bad=0
  while IFS= read -r _cc_m
  do
    _cc_up=$(printf '%s' "$_cc_m" | tr 'a-z' 'A-Z')
    if printf '%s\n' "$_cc_up" | grep -q '^CO.*AUTHOR.*BY'
    then
      _cc_val=${_cc_m#*:}
      _cc_k=$(printf '%s\n' "$_cc_val" | _cc_key)
      if [ -n "$_cc_k" ]
      then
        _cc_hit=no
        for _cc_a in $_cc_allowed
        do
          if [ "$_cc_a" = "$_cc_k" ]
          then
            _cc_hit=yes
            break
          fi
        done
        if [ "$_cc_hit" = no ]
        then
          printf 'coauthor-guard: unrecognized co-author line: %s\n' "$_cc_m" >&2
          _cc_bad=1
        fi
      fi
    fi
  done
  return $_cc_bad
}
