# Changelog

A dated, heavily cross-referenced record of what changed in the fabric. Each
entry summarizes the day's work by feature area and links to the subsystem and
reference docs for full detail.

| Date | Highlights |
|------|-----------|
| [2026-06-21](2026-06-21.md) | SDN egress/IPAM/cross-host VXLAN; fleet health + fixes (`ocf-health`); cross-OS package management (`ocf-platform`); bulk streaming transport + zstd; encrypted WireGuard underlays split into three isolated planes (`wg-mgmt`/`wg-data`/`wg-lb`) with control unified over `wg-mgmt`; capability-based placement; measured latency, reachability, relays, and weighted routing; live LB ↔ workload/autoscaling-group association |

For the per-subsystem deep dives, see [`../subsystems/`](../subsystems/); for the
endpoint shapes, the [REST API reference](../reference/rest-api.md).
