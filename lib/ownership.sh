# Sourced by bin/omarchy-cast-setup and bin/omarchy-cast-uninstall: the
# shared paths UniCast installs to, and how to tell that a file at one of
# them is UniCast's before replacing or deleting it.

BIN=$HOME/.local/bin/omarchy-castd
CONFIG=${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/cast
# SHA-256 of the binary the setup script installed: the proof that $BIN is
# ours, checked before setup replaces it and before uninstall deletes it.
OWNED=$CONFIG/installed-binary.sha256
UNIT=omarchy-castd.service
UNIT_FILE=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$UNIT
# Same line as UNIT_MARKER in src/client.rs.
UNIT_MARKER='# Written by UniCast (universal-cast); omarchy-cast-uninstall removes it.'
# Build cache. Setup marks it only in the same step that creates it, so the
# marker proves setup made the folder; uninstall deletes it only when marked.
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/universal-cast
CACHE_MARKER=$CACHE/.created-by-unicast-setup
# Where releases before 0.1.6 built. Read only, to recognise their binary;
# never written or deleted, since nothing proves who created that folder.
LEGACY_BUILD=${XDG_CACHE_HOME:-$HOME/.cache}/omarchy-cast/target/release/omarchy-castd

sha() { sha256sum -- "$1" 2>/dev/null | cut -d' ' -f1; }

# $BIN is a regular file whose hash is the one setup recorded or, for
# installs from before that record, the build it was copied from.
bin_is_ours() {
  [[ -f $BIN && ! -L $BIN ]] || return 1
  local current built=$LEGACY_BUILD
  current=$(sha "$BIN")
  [[ -s $OWNED && $current == "$(cat -- "$OWNED")" ]] && return 0
  [[ -f $built && ! -L $built && $current == "$(sha "$built")" ]]
}

# $UNIT_FILE is a regular file the client wrote: it starts with the marker,
# or it is byte-for-byte a unit from a release before the marker existed.
unit_is_ours() {
  [[ -f $UNIT_FILE && ! -L $UNIT_FILE ]] || return 1
  [[ $(head -n1 -- "$UNIT_FILE") == "$UNIT_MARKER" ]] && return 0
  # Units written before the marker: exactly one of the old templates.
  local path tail head tail0 tail1
  head=$'[Unit]\nDescription=omarchy-cast media daemon\n\n[Service]\nType=simple\n'
  tail0=$'Restart=on-failure\nRestartSec=1\n\n[Install]\nWantedBy=default.target'
  tail1=$'Restart=on-failure\nRestartSec=1\nUMask=0077\nTasksMax=1024\nMemoryMax=4G\nOOMScoreAdjust=200\nKeyringMode=private\n\n[Install]\nWantedBy=default.target'
  for path in "$BIN" /usr/bin/omarchy-castd; do
    for tail in "$tail0" "$tail1"; do
      [[ $(cat -- "$UNIT_FILE") == "$head"'ExecStart="'"$path"$'"\n'"$tail" ]] && return 0
    done
  done
  return 1
}
