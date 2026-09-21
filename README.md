# Swaddle

**NOTE: This Project Has Been Moved To [Codeberg](https://codeberg.org/templetr0n)**

Swayidle inhibitor with automatic detection for audio / video and prevent your system from sleeping.

## Overview

The main function of this project is to keep any sway based WM from going into an idle state when consuming media. Swaddle will monitor the dbus running daemon and based on values it sees in `Playback Status` will correctly cause idling or inhibition. The idea is to remove the need for a dedicate button in your swaybar config to enable and disable the inhibiting of swayidle.

## Installation

AUR:
```bash
paru -S swaddle
```

### Building from source

* Clone the repo and execute

   ```bash
   just build_release
   ```

* You can move the binary into your `$PATH` or run directly

## Post-Install

To integrate swaddle with Sway/Hyprland/River, add the following line to your Sway/Hypr configuration:

* Sway:

```conf
# Swaddle configuration
exec_always --no-startup-id /usr/local/bin/swaddle &
```

* Hyprland:

```conf
# Swaddle configuration
exec = /usr/local/bin/swaddle &
```

Then reload your configuration or restart Sway/Hyprland.

### Configuration File (Required)

Swaddle reads its configuration from `$HOME/.config/swaddle/config.toml`.
There are no built-in defaults — create the file before running swaddle:

```toml
debug = false

[server]
inhibit_duration = 25
sleep_duration = 5

[ha]
host = "http://192.168.1.1:8123"
token = "your-long-lived-access-token"
entity = "switch.my_speaker"
enabled = true

[swayidle]
config_path = "/home/you/.config/sway/swayidle.conf"
enabled = true
```

The `ha` section is optional — omit it to disable speaker control.  

| Name | Value | Explaination |
| ---- | ----- | ------------ |
|debug|boolean|should swaddle be run in debug mode|
|server|table|includes the options to tweak how swaddle operates||
|server.inhibit_duration|integer|number of seconds to inhibit per cycle|
|server.sleep_duration|integer|number of seconds to wait between checks|
|ha|table|optional Home Assistant speaker control section||
|ha.host|string|Home Assistant base URL, e.g. `http://192.168.1.1:8123`|
|ha.token|string|Home Assistant long-lived access token|
|ha.entity|string|entity id of the speaker switch|
|ha.enabled|boolean|enable / disable speaker control|
|swayidle|table|swayidle process management section||
|swayidle.config_path|string|path to the swayidle config file to run with|
|swayidle.enabled|boolean|enable / disable swayidle management|

---
