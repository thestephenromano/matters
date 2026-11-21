# Syncing your devices

Matters keeps your tasks on your device. Sync is optional: turn it on in **Sync**, from the sidebar or from Preferences, to keep several of your devices in step through a relay.

## What the relay sees

Your tasks are encrypted and signed on your device before they leave it, with keys only your devices hold. The relay stores them and passes them on, but it cannot read them, change them unnoticed, or make up new ones.

The relay *can* see:
- how much you store, and when your devices sync;
- how many devices you use;
- the network addresses they connect from.

It can also refuse to store or pass on your changes, or hold back new ones. When it does, Matters tells you that it can't reach the relay or that the relay refused a change, and keeps your changes on this device.

## Starting, and your recovery kit

**Start syncing from this device** needs the relay's address (`https://…`). Matters then:
1. takes a copy of your database, kept beside it;
2. makes every task you already have part of the new collection;
3. shows your **recovery kit**: the relay's address, the collection's ID, and 24 recovery words.

**Write the recovery kit down, and keep it safe.** If you lose every device, it is the only way to get your tasks back: nobody else holds it, not the relay's operator and not Matters. Anyone who has it can read and change your tasks, so keep it as you would a password.

Some limits apply:
- Each change of membership seals that period's key for recovery. A period whose key was never sealed this way can't be read back from the words alone. Matters says so if it happens.
- A new relay doesn't know your collection until its operator sets it up. Until then, Sync shows the command to give them. If you run the relay yourself, see [Self-hosting](SELF_HOSTING.md).

## Adding a device

On a device that syncs, choose **Add a device**. It shows an invitation: copy it to the new device, open Sync there, paste it under **Join from another device**, and choose **Join**.

The invitation works once, within an hour. Anyone who sees it can ask to join, so share it only with your own devices. The new device must not hold tasks of its own yet. If it does, set them aside first, and add them after it has joined.

## When a device is lost

The admin device (the one that started syncing) can **Remove** a lost device. The removed device keeps the tasks it had, but receives nothing new.

If you lose every device, install Matters on a new one, open Sync, and use **Recover after losing every device**. You will need the relay's address, the collection ID and the 24 words. The new device becomes the admin. Your other devices, if you find them, join again through an invitation.

## Changes not yet on the relay

Sync shows how many changes are saved on this device but not yet on the relay. They are safe on this device and are sent when the relay can be reached. They are **not** backed up anywhere else until then.

**Stop syncing on this device** keeps every task here. If changes have not reached the relay yet, Matters says so and asks before it stops. Those changes would then never reach your other devices. A copy of the database is kept beside it, in case you change your mind.
