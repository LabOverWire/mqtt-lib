#!/usr/bin/env bash
set -euo pipefail

KERNEL="${1:?usage: $0 <kernel release, e.g. 6.17.0-1008-gcp>}"

sudo systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer
while sudo fuser /var/lib/dpkg/lock-frontend /var/lib/dpkg/lock >/dev/null 2>&1; do
    sleep 5
done

if [ ! -e "/boot/vmlinuz-${KERNEL}" ]; then
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "linux-image-${KERNEL}" "linux-modules-${KERNEL}"
fi
sudo apt-mark hold linux-gcp linux-image-gcp linux-headers-gcp >/dev/null

submenu=$(sudo grep -o "gnulinux-advanced-[^']*" /boot/grub/grub.cfg | head -1)
entry=$(sudo grep -o "gnulinux-${KERNEL}-advanced-[^']*" /boot/grub/grub.cfg | head -1)
if [ -z "$submenu" ] || [ -z "$entry" ]; then
    sudo update-grub >/dev/null 2>&1
    submenu=$(sudo grep -o "gnulinux-advanced-[^']*" /boot/grub/grub.cfg | head -1)
    entry=$(sudo grep -o "gnulinux-${KERNEL}-advanced-[^']*" /boot/grub/grub.cfg | head -1)
fi
if [ -z "$submenu" ] || [ -z "$entry" ]; then
    echo "no grub entry for ${KERNEL}" >&2
    exit 1
fi

echo "GRUB_DEFAULT=\"${submenu}>${entry}\"" | sudo tee /etc/default/grub.d/99-pin-kernel.cfg >/dev/null
sudo update-grub >/dev/null 2>&1
grep -h "^GRUB_DEFAULT" /etc/default/grub.d/99-pin-kernel.cfg
echo "pinned ${KERNEL}; running $(uname -r)"
