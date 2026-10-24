# Waking the phone without Google: UnifiedPush

A phone that has stopped Spike Connect (swiped away, killed to save memory, or after a restart) cannot hear the relay.
Apps usually fix that with Google's push service. Spike uses the open **UnifiedPush** standard instead, so no Google or
Apple account or service is involved. See ADR 0028.

## How it works
1. You install a **push app** on the phone (for example **ntfy**) and, optionally, run your own push server.
2. In Spike Connect, **Settings, Wake-ups without Google**: turn it on. The push app gives Spike a private web address (a
   topic on the push server). Spike leaves that address with your relay.
3. When a call or message waits at the relay for your phone and the phone is not connected, the relay sends **one empty
   message** to that address. It carries only a fixed word, never a name or text.
4. The push app wakes Spike, Spike connects to the relay, reads what is waiting, and a call rings.

## What each side learns
- **The push server** (yours, or ntfy.sh) learns the *time* of each wake-up for your phone. Not who called, not any text.
  Run your own ntfy server if you do not want a public one to see that.
- **Your relay** learns the push address and sends to it, as it already knew when your phone connects. It sends at most one
  wake-up per device every 5 seconds, to `https://` addresses on the public Internet only (never to its own network).

## What the phone needs
- A UnifiedPush app installed. If several are installed, Spike uses the one Android lists first.
- **"Keep receiving after a restart"** turned on in Settings. A wake-up can only help if the app can open its engine by
  itself, which needs the passphrase sealed under the phone's hardware key. Without it a wake-up can only show "something is
  waiting"; you open the app and enter your passphrase.
- Battery settings that let the push app run in the background (see **Calls on this phone** in Settings).

## Not covered
- An app that was **force-stopped** from Android's settings receives nothing, from any push system, until it is opened.
- Real push apps were not available where this was built: the relay side is tested against a local web address, and the
  phone side was tested by delivering the wake-up from the command line to an app whose process had been killed.

## Relay flags
`--no-push` turns the feature off. `--push-min-interval-secs N` changes the 5-second spacing. `--push-allow-insecure` also
allows `http://` and private addresses: for tests and a push server on your own network only.
