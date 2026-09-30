# Cloud Mac deployment plan

Source: MSG-70893 (card AMUX-5363), plus Ethan's follow-up on 2026-09-30.
Written 2026-09-30 by amux-cloud. Prices checked the same day.

## Goal

Get a customer live on amux in one day with no hardware purchase:

1. Rent a macOS machine in the cloud that stays up 24/7.
2. Put it on a Tailscale tailnet that both our side and the customer's side join.
3. Run amux on it, with every action attributed to the person who took it.
4. Give both sides remote screen control (see the screen, click, type).
5. Back up the whole system so it can move later, for example onto a dedicated
   Mac Studio the customer buys, without rebuilding by hand.

## Provider choice

All of these are real Apple hardware. Apple's macOS license requires a 24-hour
minimum lease, so every provider bills by the day or month, never by the minute.

| Provider | Machine | Price | Region | Notes |
|---|---|---|---|---|
| Scaleway | Mac mini M4, 16 GB, 256 GB | €149/mo (€0.22/h) | Paris only | Cheapest current M4. VNC built in. |
| Scaleway | Mac mini M4, 32 GB, 1 TB | €199/mo (€0.29/h) | Paris only | Best fit for amux plus a few agents. |
| Scaleway | Mac mini M4 Pro, 64 GB, 2 TB | €335/mo (€0.49/h) | Paris only | Heavy agent load. |
| MacStadium | Mac mini M4, 16 GB, 256 GB | $119 to $149/mo | US (Atlanta, Las Vegas) and EU | 24x7 support is 10 to 20% extra. |
| MacStadium | Mac mini M4, 24 GB, 512 GB | $199 to $249/mo | US and EU | |
| AWS EC2 | mac-m4.metal, 24 GB | about $1.23/h, about $900/mo | Most regions | Fits if the customer must stay inside AWS. Six times the price. |

**Recommendation: MacStadium Mac mini M4 (24 GB) for a US customer, Scaleway M4-M
(32 GB) for an EU one.** The machine sits close to the people using it, so screen
sharing feels responsive, and it costs about $200/month. AWS only makes sense
when the customer's security review requires AWS.

16 GB is too tight once amux, a browser and two or three Claude Code panes run
together. Start at 24 or 32 GB.

Can it stay up 24/7? Yes. These are dedicated physical machines with no idle
shutdown. The risk is a macOS update rebooting it, so turn off automatic OS
updates and let it boot straight into an unlocked account (step 2 below).

## Day-one runbook

Target: about 4 hours of our time, most of it waiting on the provider.

### 1. Provision (30 min)

- Order the machine. Scaleway is instant through its console or API. MacStadium
  can take up to a business day, so order the day before.
- Note the admin user, the password and the public IP.

### 2. Harden and set up macOS (45 min)

- Change the admin password and store it in the amux vault.
- Turn on FileVault only if the provider gives console access for the unlock
  prompt after a reboot. Otherwise a reboot hangs at the unlock screen with
  nobody able to reach it. Scaleway and MacStadium both provide that access.
- Disable automatic macOS updates. Updates get scheduled and applied by us.
- Set energy settings: never sleep, restart after power failure.
- Create one macOS user per person (`ethan`, `customer-name`). Separate
  accounts are what make attribution work at the OS level (see below).

### 3. Tailscale (20 min)

- Install Tailscale and log it in to a tailnet we own.
- Invite the customer's people as tailnet users. Tailscale's free plan covers
  3 users, and the paid plan is about $6 per user per month.
- ACLs: both sides may reach this machine on screen sharing (5900), SSH (22)
  and amux (8824). Nothing else on our tailnet is reachable for them.
- Turn on Tailscale SSH so shell access is also tied to a tailnet identity.
- Once Tailscale works, close the public screen-sharing and SSH ports in the
  provider firewall. After that the machine is reachable only over the tailnet.

### 4. Remote screen (15 min)

- Turn on Screen Sharing (System Settings, General, Sharing) and limit it to
  the named users.
- Connect over the tailnet with the built-in Screen Sharing app
  (`vnc://<tailnet-name>`). On macOS 14 and later, "High Performance" mode gives
  smooth video on a good connection.
- Two people can watch and control one screen at once. Each person can also log
  in to their own separate desktop session with their own macOS account, so
  they can work in parallel without taking the mouse from each other.

### 5. amux (60 min)

- Install from a reviewed checkout: `install.sh`, then `make install-cli`.
- Set `~/.amux/server.env` with only the credentials this customer needs. Never
  copy our server.env over.
- Serve the dashboard on 8824 to the tailnet only.
- Create the customer's workers and board.

### 6. Backup (30 min, see next section)

### 7. Walkthrough with the customer (60 min)

- Each person joins the tailnet, opens the dashboard and the screen, and takes
  one action. Then we read the board log and confirm each action shows the
  right name.

## Attribution: who did what

Three layers, in the order to rely on them:

1. **amux**: every board mutation, message and send is stamped server-side with
   the acting session (`X-Amux-Session`), and the board log records it. **Open
   question to check before promising "full provenance":** amux attributes
   actions to a worker session today, not to a human. Two humans using the same
   dashboard look the same in the log. We need per-person sign-in on the local
   dashboard, or a per-person identity header set at the tailnet edge (Tailscale
   Serve passes `Tailscale-User-Login`), recorded on every mutation. File this
   as its own amux card before the first customer depends on it.
2. **Tailscale**: every connection is tied to a tailnet user, and the admin
   console logs it. This tells us who connected, not what they did.
3. **macOS**: separate user accounts mean files, shell history and the unified
   log each carry the account name.

## Backup and portability

The goal is to lift the whole setup onto other hardware later, such as a Mac
Studio at the customer's office.

A disk-image clone does not work for this. Apple silicon Macs cannot reliably
boot a cloned system volume on a different machine. The supported path is
Migration Assistant, which reads a Time Machine backup and rebuilds apps,
users, settings and data on the new Mac.

So run two backups:

1. **Time Machine, for moving machines.** Point it at a network share on the
   tailnet (an SMB share on a Mac or NAS we control), hourly. To move: set up
   the new Mac, run Migration Assistant, choose that backup. This restores every
   user account, app and setting.
2. **restic to Backblaze B2, for disaster recovery.** The same setup this box
   already uses (see `~/Dev/CLAUDE.md`): `~/`, `~/.amux`, `~/.claude`, every 6
   hours, 7 daily, 4 weekly and 6 monthly snapshots. Use a separate bucket and
   key per customer. Cost is well under $1/month at these sizes.

Also keep a **bootstrap script in the repo** (`Brewfile` plus the steps above as
a script), so a clean machine can be rebuilt without either backup. It also
documents exactly what the machine contains.

Test the move before a customer needs it: restore the Time Machine backup onto
a second rented Mac for a day (about $10) and check that amux, the workers and
Tailscale come back up.

## Monthly cost, one customer

| Item | Cost |
|---|---|
| Mac mini M4, 24 to 32 GB | about $200 |
| Tailscale, if more than 3 users | about $6 per user |
| Backblaze B2 | under $1 |
| **Total** | **about $200 to $230** |

## Decisions needed from Ethan

- **Spending:** approve the first rental (about $200/month) and which provider,
  once the customer's region is known.
- **Tailnet:** one tailnet per customer, or a shared tailnet with strict ACLs.
  Recommendation: one per customer, so nothing on ours is ever reachable by them.

## Next steps

1. Card: per-person attribution on the amux dashboard (the open question
   above). This blocks any promise of "full provenance".
2. Card: bootstrap script and Brewfile in the repo.
3. After spending approval: rent one Mac and run the day-one runbook against it
   ourselves before the first customer.

## Sources

- Scaleway Apple silicon pricing: https://www.scaleway.com/en/pricing/apple-silicon/
- AWS EC2 Mac instances: https://aws.amazon.com/ec2/instance-types/mac/
- mac-m4.metal on-demand price: https://instances.vantage.sh/aws/ec2/mac-m4.metal?currency=USD
- MacStadium pricing: https://macstadium.com/pricing
