#!/usr/bin/env python3
"""Post-cleanup validation only; never fits or modifies any acquired evidence."""
import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "dense-count-validation1"
TRAINING = ROOT / "dense-count-training3"
FIT_SHA = "27a4219e358771bf72d7b399b20bba10b8e6bf54f8e4c82e704c49464c20b84a"
CHECKER_SHA = "dc6f607cda8eaa1ed1810029f4899dabdff1b8e9d1445b9af47ab50493889b5a"


def main():
    import hashlib
    checker = ROOT / "dense-count-calibration.py"
    if hashlib.sha256(checker.read_bytes()).hexdigest() != CHECKER_SHA:
        raise ValueError("frozen checker changed")
    spec = importlib.util.spec_from_file_location("frozen_calibration", checker)
    v = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(v)

    def checked(ref):
        path = (ROOT / ref["path"]).resolve()
        v.need(path.is_relative_to(ROOT) and path.is_file(), "evidence outside root/missing")
        v.need(v.digest(path) == ref["sha256"], "evidence hash mismatch")
        return path

    v.need((RUN / "terminal.log").read_text().strip() == "terminal=0", "outer run failed")
    completion = v.read(RUN / "completion.json")
    v.need(completion["kind"] == "validation_run_complete" and
           type(completion["terminal"]) is int and completion["terminal"] == 0,
           "invalid final completion")
    manifest = TRAINING / "manifest.json"
    v.need(completion["manifest_sha256"] == v.digest(manifest), "wrong completion manifest")
    binding_path = checked(completion["validation_binding"])
    v.need(binding_path == RUN / "validation-binding.json", "wrong validation binding")
    binding = v.read(binding_path)
    v.need(binding["kind"] == "frozen_fit_validation_acquisition", "wrong binding kind")
    v.need(checked(binding["manifest"]) == manifest, "wrong bound manifest")
    fit = checked(binding["fit"])
    v.need(fit == TRAINING / "frozen-fit.json" and v.digest(fit) == FIT_SHA, "wrong reviewed fit")
    harness = [checked(ref) for ref in binding["harness"]]
    v.need(set(harness) == {ROOT / "acquire-dense-count-validation.py",
                           ROOT / "run-dense-count-validation.sh"} and len(harness) == 2,
           "wrong validation harness")
    ids = v.ids("holdout") + v.ids("existing_validation")
    v.need(binding["cell_ids"] == ids and set(completion["cells"]) == set(ids),
           "missing/extra validation cells")
    paths = []
    for cell_id in ids:
        path = checked(completion["cells"][cell_id])
        v.need(path == RUN / (cell_id + ".json"), "wrong cell path")
        cell = v.read(path)
        acquisition = v.read(checked(cell["acquisition_receipt"]))
        v.need(acquisition["validation_binding"] == completion["validation_binding"],
               "cell acquired under different validation binding")
        paths.append(path)
    result = v.validate(ROOT, manifest, fit, FIT_SHA, paths)
    result["finalization"] = {"completion_sha256": v.digest(RUN / "completion.json"),
                              "finalizer_sha256": v.digest(Path(__file__)),
                              "terminal_sha256": v.digest(RUN / "terminal.log")}
    output = RUN / "validation-results.json"
    with output.open("x") as stream:
        json.dump(result, stream, indent=2, allow_nan=False)
        stream.write("\n")
    print(json.dumps({"output": str(output), "sha256": v.digest(output)}))


if __name__ == "__main__":
    main()
