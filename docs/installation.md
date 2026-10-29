# Installation and service

[Back to XRealD](../README.md)

Run the installer from the extracted bundle or source directory. Both the
[prebuilt](../README.md#install-prebuilt-binaries) and
[source](../README.md#build-from-source) paths use the same installer.
Python 3 is needed only to install/remove; the running daemon is native Rust.

## USB networking

Connect the glasses with a USB data cable. Their **NCM (`cdc_ncm`) network
interface** needs a host IPv4 address such as `169.254.2.10/24`, with no gateway
or DNS. Configure this USB connection using your system's network settings;
keep it off the default route. Use NCM rather than the glasses' separate ECM interface.

The glasses are reached at `169.254.2.1`, TCP 52998 (IMU) and 52999 (control).
XRealD selects the source address from the interface belonging to USB `3318:043e`.
It does not configure networking. A working DisplayPort picture alone does not
prove the USB data/network connection is ready.

## Button permissions

An active desktop session may already grant access to XREAL input nodes.
For persistent access, including headless startup, install a rule for your user:

```sh
sudo python3 -B install.py permissions --user "$(id -un)"
sudo python3 -B install.py permissions --user "$(id -un)" --apply
sudo udevadm control --reload-rules
```

Then disconnect and reconnect XREAL. The rule grants read access only to
`3318:043e` input nodes. It does not add you to the global `input` group, grab
devices or affect other keyboards. Existing rules are never overwritten.
Quick Button TCP events also require the USB network connection described above.

## Start and stop

```sh
systemctl --user daemon-reload
systemctl --user enable --now xreald.service
systemctl --user status xreald.service
```

The service normally starts at login. For startup before login, opt in with
`sudo loginctl enable-linger "$USER"`. This also keeps your other enabled user
services running after logout. The installer does not enable lingering itself.

```sh
systemctl --user stop xreald.service
systemctl --user start xreald.service
```

Files: `~/.local/bin/xreald`, `~/.local/bin/xrealctl`,
`~/.config/systemd/user/xreald.service`. The receipt is under
`~/.local/state/xreald-install`.
The private socket is `$XDG_RUNTIME_DIR/xreald/control.sock`, mode `0600`,
inside a `0700` directory. Applications using it must run as the same user.

## Upgrade from 0.1.2

From the new bundle, replace the existing installation as your normal user.
The first command checks the new binaries against their source and checksums
before stopping the old service. The sequence stops if a command fails:

```sh
python3 -B install.py verify && \
systemctl --user disable --now xreald.service && \
python3 -B install.py remove --apply && \
systemctl --user daemon-reload && \
python3 -B install.py install --apply && \
systemctl --user daemon-reload && \
systemctl --user enable --now xreald.service
```

This keeps your USB network settings and input rule. Connected applications need
to reconnect to the service. Removal checks the old installation receipt and
refuses changed files; a manually installed copy needs its original uninstall
procedure. Keep the old bundle if you want to return to it.

`verify` is read-only and also accepts `--bundle` for a locally built bundle.

## Problems and fixes

- **No control/IMU connection:** check the USB NCM address, cable and the
  `discovery`/`control` fields in `~/.local/bin/xrealctl status`.
- **Input permission errors:** install the scoped rule above and reconnect.
- **IMU is offline with no subscribers:** expected; start `watch imu` to request it.
- **Wearing state is unknown:** take the glasses off and put them on. The state
  stays unknown until the glasses send an event on the current control connection.
- **Several matching glasses:** connect only one pair at a time.
- **Service fails:** use `journalctl --user -u xreald.service -n 30 --no-pager`.
- **Installer finds existing files:** it will not migrate an older/manual setup.
  Preserve that setup and remove it through its original installation procedure.

## Remove

```sh
systemctl --user disable --now xreald.service
python3 -B install.py remove
python3 -B install.py remove --apply
systemctl --user daemon-reload
```

Removal checks the installed files against the receipt and refuses changed files.
Other files and your linger/network settings remain. If this installer created
the input rule, remove it separately:

```sh
sudo python3 -B install.py remove-permissions --user "$(id -un)"
sudo python3 -B install.py remove-permissions --user "$(id -un)" --apply
sudo udevadm control --reload-rules
```

Reconnect XREAL to apply the rule removal.
