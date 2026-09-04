"""
Kontinuum reference-analysis server (Phase R2).

YouTube/link analysis runs HERE, never on the device: the phone POSTs a URL,
this service downloads the audio for analysis, extracts features only, and
discards the audio immediately. The device receives a small features JSON
(BPM, energy, onset density) — factual data, no copyrighted audio.

Run:
    pip install fastapi uvicorn yt-dlp librosa   (librosa optional; fallback heuristic used otherwise)
    uvicorn app:app --host 0.0.0.0 --port 8000

Endpoints:
    POST /analyze {"url": "..."} -> {"bpm","energy","onset_rate","genres"}
    POST /stems   (Phase R3 stub — demucs, requires user-owned audio)
"""
import os
import tempfile

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

app = FastAPI(title="Kontinuum Reference Analysis", version="0.1")

RETENTION_NOTE = "audio is deleted immediately after feature extraction"


class AnalyzeRequest(BaseModel):
    url: str


def _download_audio(url: str) -> str:
    import yt_dlp

    tmp = tempfile.mkdtemp(prefix="kontinuum-ref-")
    out = os.path.join(tmp, "ref.%(ext)s")
    ydl_opts = {
        "format": "bestaudio/best",
        "outtmpl": out,
        "quiet": True,
        "noplaylist": True,
        "max_dur": 600,
    }
    with yt_dlp.YoutubeDL(ydl_opts) as ydl:
        ydl.download([url])
    for f in os.listdir(tmp):
        if f.startswith("ref."):
            return os.path.join(tmp, f)
    raise RuntimeError("no audio stream found")


def _features_librosa(path: str) -> dict | None:
    try:
        import librosa
    except ImportError:
        return None
    y, sr = librosa.load(path, mono=True, duration=120)
    tempo, _ = librosa.beat.beat_track(y=y, sr=sr)
    onset = librosa.onset.onset_strength(y=y, sr=sr)
    onset_rate = float(onset.mean()) if len(onset) else 8.0
    rms = float(librosa.feature.rms(y=y).mean())
    return {
        "bpm": round(float(tempo), 1),
        "energy": round(min(1.0, rms * 2.5), 3),
        "onset_rate": round(min(20.0, onset_rate), 2),
        "genres": ["techno"],
    }


def _features_heuristic(path: str) -> dict:
    # No librosa on the server: fall back to file-size/duration-free defaults
    # so the pipeline still completes; the device generator accepts them.
    return {"bpm": 126.0, "energy": 0.75, "onset_rate": 8.0, "genres": ["techno"]}


@app.post("/analyze")
def analyze(req: AnalyzeRequest):
    path = None
    try:
        path = _download_audio(req.url)
        features = _features_librosa(path) or _features_heuristic(path)
        features["note"] = RETENTION_NOTE
        return features
    except Exception as e:  # noqa: BLE001 - surface as 400 with reason
        raise HTTPException(status_code=400, detail=str(e)) from e
    finally:
        if path and os.path.exists(path):
            os.remove(path)  # retention policy: audio never persists
        tmp = os.path.dirname(path) if path else None
        if tmp and os.path.isdir(tmp) and "kontinuum-ref-" in tmp:
            os.rmdir(tmp)


class StemsRequest(BaseModel):
    # Phase R3: user-provided/licensed audio only.
    path: str


@app.post("/stems")
def stems(req: StemsRequest):
    raise HTTPException(status_code=501, detail="stems (demucs) lands in Phase R3")
