# Alarm at Game End

A small Windows desktop app for alarms that can wait until your game is over.

Set an alarm, choose whether it should delay while a tracked game is active, and get a desktop notification plus an in-app popup when it fires. The app includes a default alarm sound and can use a custom sound file.

## Features

- Schedule and manage multiple pending alarms with short labels.
- Accept alarm times like `1:25`, `01:25`, `125`, and `0125`.
- Delay alarms while League of Legends or Counter-Strike 2 is in an active game.
- Use desktop notifications, a persistent in-app popup, and bundled or custom alarm sounds.
- Save settings, pending alarms, and recent logs between launches.
- Pair with the Discord bridge to receive remote alarm requests.
- Manage Discord pairing, owner-created invites, and allowed users from a dedicated Discord page. Polling starts automatically after pairing, temporary pairing codes are hidden once inactive, and new pending invites are surfaced on the Discord page.

## Game Detection

League of Legends is detected from the local lockfile, with a manual override available in the app. The lockfile password is read only while the app is running and is not stored.

Counter-Strike 2 uses Game State Integration through a local listener. Install the GSI config from the app so CS2 can report active map state.

If game detection is unavailable when an alarm is due, the alarm fires instead of waiting indefinitely.
