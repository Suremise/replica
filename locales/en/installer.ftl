# The MSI's own text (installer/replica.wxs), put in by scripts/package.ps1 when it builds the MSI.
# One line each, no placeables: the installer can't format Fluent. Windows' own installer windows
# use Windows' own text.

# Shown when a newer Replica is already installed.
installer-downgrade = A newer version of Replica is already installed.
# The Start menu shortcut's description (its tooltip).
installer-shortcut-description = Versioned backups and mirrors
