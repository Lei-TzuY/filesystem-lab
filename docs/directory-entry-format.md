# Directory entry record format v1

Directory namespace persistence starts with an independently versioned record codec. This document fixes the byte-level contract for one directory entry without yet assigning a filesystem region or changing filesystem format v4.

## Record layout

All integer fields are little-endian. The fixed header is 40 bytes, followed immediately by the UTF-8 name bytes.

| Offset | Size | Field | v1 rule |
| ---: | ---: | --- | --- |
| 0 | 4 | magic | ASCII `DNT1` |
| 4 | 2 | version | `1` |
| 6 | 2 | flags | must be zero |
| 8 | 4 | total length | header plus name bytes |
| 12 | 2 | name length | UTF-8 byte length |
| 14 | 2 | reserved | must be zero |
| 16 | 8 | parent inode | non-zero inode identifier |
| 24 | 8 | target inode | non-zero inode identifier |
| 32 | 4 | CRC-32 | IEEE CRC-32 over the entire record with this field treated as zero |
| 36 | 4 | reserved | must be zero |
| 40 | variable | name | UTF-8 bytes, no terminator |

The decoder consumes exactly one record. Extra trailing bytes are rejected rather than silently interpreted as part of a later format.

## Name contract

A v1 name is one path component encoded as UTF-8 and limited to 255 bytes. Empty names, `.` and `..`, slash, and NUL are invalid. The limit is defined in bytes rather than Unicode scalar values so the on-disk bound is deterministic.

The record preserves the parent and target inode identifiers but deliberately does not duplicate inode kind. Cross-layer validation that the parent exists and is a directory, and that the target inode exists, belongs to the future durable directory-table loader/fsck integration.

## Corruption semantics

A torn fixed header or payload is reported as an unexpected end of input. Bad magic/version, non-zero flags or reserved fields, inconsistent lengths, CRC mismatch, zero inode identifiers, invalid UTF-8, invalid component names, and trailing bytes are corruption and are rejected deterministically.

## Scope boundary

This record version is independent of filesystem format v4. This milestone does **not** reserve directory blocks, persist a directory-table image, journal namespace changes, define rename/unlink crash ordering, or add link-count/reachability rules. Those require an explicit later filesystem-format revision and focused crash-consistency tests.
