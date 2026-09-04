# Kontinuum reference-analysis server

YouTube/link analysis runs on this server, **never on the device** (PLAN §2.4:
device/cloud split). The device POSTs a URL and receives a small features
JSON — factual, non-copyrightable data. Audio is deleted immediately after
extraction (retention policy enforced in `app.py`).

```
pip install -r requirements.txt
uvicorn app:app --host 0.0.0.0 --port 8000
```

Point the app at it: Settings → ANALYSIS SERVER → `http://<server>:8000`,
then use the link icon to paste a reference URL.

Legal note: downloading violates YouTube ToS §2T even for analysis; the
mitigation is operator-owned infrastructure, ephemeral storage, no
redistribution, features-only output. See docs/RESEARCH/instruments-and-reference.md §2.1.
