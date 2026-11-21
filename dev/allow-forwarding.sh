#!/bin/sh
# The image turns port forwarding off; ssh1 and ssh2 allow it so tunnels can be tried (ssh3 keeps it off).
sed -i 's/^AllowTcpForwarding no/AllowTcpForwarding yes/; s/^GatewayPorts no/GatewayPorts clientspecified/' /config/sshd/sshd_config
