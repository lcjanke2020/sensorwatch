<!--
  incident.md — one open incident per rule (incidents/open/<rule>.md), created
  and maintained by open_incident.py. Machine-local, not in git. Cap: 80 lines
  — the maintenance pass and state_summary flag an over-cap file.

  The `- key: value` header block is machine-maintained (open_incident writes it;
  state_summary reads it). The Events log is append-only. Notes are yours: agent
  and human judgment goes there. When the rule clears, --close appends a closing
  note and moves this file to incidents/closed/.
-->

# Incident: {rule}

- rule: {rule}
- severity: {severity}
- classification: {classification}
- opened: {opened}
- snooze_until: {snooze_until}
- events: {events}
- status: {status}

## Events

<!-- one line per delivered event: <id> @ <timestamp>  <state>  value=<value> <unit> -->
{events_block}

## Notes

<!-- Triage judgment, baseline comparison, and the decision (benign | anomaly |
     incident). A continuation of a still-firing incident inside its snooze
     window gets a one-line note here, not a re-investigation. -->
