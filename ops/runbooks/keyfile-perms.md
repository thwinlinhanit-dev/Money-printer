# keyfile-perms (P2)

A credential/cert file is not `0600` (OPS-7).

## Symptoms
- Perms alert naming the offending file.

## Diagnosis
- `ls -l` the path; a key readable by group/other is a leak risk.

## Remediation
- `chmod 0600 <file>`; confirm ownership is the service user only.
- Never commit the file (PD-2). If it was ever world-readable on a shared host,
  rotate the key.

## Escalation
Evidence of exposure ⇒ rotate immediately and audit access logs.
