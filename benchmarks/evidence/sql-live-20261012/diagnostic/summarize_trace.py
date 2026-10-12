"""Export only owned backend rows from an attached Metal trace XML export."""
import json
import sys
import xml.etree.ElementTree as ET

definitions = {}
rows = []
table = None
kept_tags = {"process", "pid", "start-time", "duration", "gpu-channel-name",
             "metal-nesting-level", "formatted-label", "narrative",
             "metal-event-name", "metal-command-buffer-id"}

def resolve(element):
    if element is None:
        return None
    if "ref" in element.attrib:
        return definitions[element.attrib["ref"]]
    return (element.text or "", element.attrib.get("fmt", ""))

for event, element in ET.iterparse(sys.argv[1], events=("start", "end")):
    if event == "start":
        if element.tag == "node":
            table = element.attrib["xpath"]
        continue
    if element.tag in kept_tags and "id" in element.attrib:
        value = (element.text or "", element.attrib.get("fmt", ""))
        if element.tag == "process":
            pid = element.find("pid")
            value = (resolve(pid)[0], value[1])
        definitions[element.attrib["id"]] = value
    if element.tag != "row":
        continue
    process = resolve(element.find("process"))
    if process and process[0] == "11458":
        record = {"table": table, "pid": 11458}
        for child in element:
            if child.tag in kept_tags and child.tag != "process":
                record.setdefault(child.tag, []).append(resolve(child))
        rows.append(record)
    element.clear()

print(json.dumps({"owned_backend_pid": 11458, "rows": rows}))
for suffix in ("[53]", "[54]", "[109]"):
    selected = [r for r in rows if r["table"].endswith(suffix)]
    spans = sorted((int(r["start-time"][0][0]),
                    int(r["start-time"][0][0]) + int(r["duration"][0][0]))
                   for r in selected)
    total = sum(end - start for start, end in spans)
    union = 0
    current = None
    for start, end in spans:
        if current is None:
            current = (start, end)
        elif start <= current[1]:
            current = (current[0], max(current[1], end))
        else:
            union += current[1] - current[0]
            current = (start, end)
    if current:
        union += current[1] - current[0]
    print(json.dumps({"table": suffix, "rows": len(selected),
                      "duration_sum_ns": total, "interval_union_ns": union,
                      "first_start_ns": spans[0][0] if spans else None,
                      "last_end_ns": spans[-1][1] if spans else None}), file=sys.stderr)
