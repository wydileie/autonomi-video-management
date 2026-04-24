import React, { useState, useEffect, useRef, useCallback } from "react";
import axios from "axios";
import Hls from "hls.js";

const API = process.env.REACT_APP_API_URL || "/api";
const STREAM = process.env.REACT_APP_STREAM_URL || "/stream";

const RESOLUTION_OPTIONS = [
  { value: "360p",  label: "360p  (SD, ~500 kbps)" },
  { value: "480p",  label: "480p  (SD+, ~1 Mbps)" },
  { value: "720p",  label: "720p  (HD, ~2.5 Mbps)" },
  { value: "1080p", label: "1080p (Full HD, ~5 Mbps)" },
];

// ── Styles (inline, no external CSS dependency) ───────────────────────────────
const css = {
  app:       { fontFamily: "system-ui,sans-serif", background:"#0f0f1a", color:"#e0e0e0", minHeight:"100vh", padding:"0 0 40px" },
  header:    { background:"#16213e", padding:"16px 32px", display:"flex", alignItems:"center", gap:12, borderBottom:"1px solid #1f4068" },
  logo:      { fontSize:22, fontWeight:700, color:"#4fc3f7", letterSpacing:1 },
  nav:       { marginLeft:"auto", display:"flex", gap:8 },
  navBtn:    (active) => ({ background: active ? "#1f4068" : "transparent", color: active ? "#4fc3f7" : "#aaa", border:"1px solid "+(active?"#1f4068":"#333"), borderRadius:6, padding:"6px 14px", cursor:"pointer" }),
  main:      { maxWidth:900, margin:"32px auto", padding:"0 16px" },
  card:      { background:"#16213e", borderRadius:10, padding:24, marginBottom:24, border:"1px solid #1f4068" },
  h2:        { margin:"0 0 16px", color:"#4fc3f7", fontSize:18 },
  label:     { display:"block", marginBottom:6, fontSize:13, color:"#aaa" },
  input:     { width:"100%", padding:"8px 10px", background:"#0f0f1a", border:"1px solid #1f4068", borderRadius:6, color:"#e0e0e0", fontSize:14, boxSizing:"border-box" },
  btn:       { background:"#1565c0", color:"#fff", border:"none", borderRadius:6, padding:"9px 20px", cursor:"pointer", fontSize:14, fontWeight:600 },
  btnSm:     { background:"#1565c0", color:"#fff", border:"none", borderRadius:6, padding:"5px 12px", cursor:"pointer", fontSize:13 },
  btnDanger: { background:"#c62828", color:"#fff", border:"none", borderRadius:6, padding:"5px 12px", cursor:"pointer", fontSize:13 },
  badge:     (status) => {
    const colors = { pending:"#555", processing:"#e65100", ready:"#2e7d32", error:"#b71c1c" };
    return { background: colors[status] || "#555", color:"#fff", fontSize:11, padding:"2px 8px", borderRadius:12, fontWeight:600, textTransform:"uppercase" };
  },
  checkRow:  { display:"flex", gap:12, flexWrap:"wrap" },
  checkLabel:{ display:"flex", alignItems:"center", gap:6, fontSize:13, cursor:"pointer", userSelect:"none" },
  videoWrap: { width:"100%", background:"#000", borderRadius:8, overflow:"hidden", marginTop:12 },
  video:     { width:"100%", display:"block" },
  table:     { width:"100%", borderCollapse:"collapse" },
  th:        { textAlign:"left", padding:"8px 10px", fontSize:12, color:"#888", borderBottom:"1px solid #1f4068" },
  td:        { padding:"10px 10px", fontSize:14, borderBottom:"1px solid #1a2a44", verticalAlign:"middle" },
  empty:     { textAlign:"center", color:"#555", padding:32 },
  progress:  { background:"#0f0f1a", borderRadius:4, height:6, overflow:"hidden", marginTop:4 },
  progressBar:(pct) => ({ width:pct+"%", height:"100%", background:"#1565c0", transition:"width .4s" }),
  resRow:    { display:"flex", gap:8, marginTop:8, flexWrap:"wrap" },
  resBtn:    (active) => ({ background: active ? "#1565c0" : "#1a2a44", color:"#fff", border:"1px solid "+(active?"#1565c0":"#1f4068"), borderRadius:6, padding:"4px 12px", cursor:"pointer", fontSize:13 }),
  error:     { color:"#ef9a9a", fontSize:13, marginTop:6 },
};

// ── HLS Player component ──────────────────────────────────────────────────────
function VideoPlayer({ videoId, resolution }) {
  const videoRef = useRef(null);
  const hlsRef   = useRef(null);
  const src = `${STREAM}/${videoId}/${resolution}/playlist.m3u8`;

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;

    if (Hls.isSupported()) {
      const hls = new Hls({ enableWorker: true, lowLatencyMode: false });
      hlsRef.current = hls;
      hls.loadSource(src);
      hls.attachMedia(video);
      hls.on(Hls.Events.MANIFEST_PARSED, () => video.play().catch(() => {}));
      return () => { hls.destroy(); hlsRef.current = null; };
    } else if (video.canPlayType("application/vnd.apple.mpegurl")) {
      video.src = src;
      video.play().catch(() => {});
    }
  }, [src]);

  return (
    <div style={css.videoWrap}>
      <video ref={videoRef} style={css.video} controls playsInline />
    </div>
  );
}

// ── Upload panel ──────────────────────────────────────────────────────────────
function UploadPanel({ onUploaded }) {
  const [file, setFile]         = useState(null);
  const [title, setTitle]       = useState("");
  const [desc, setDesc]         = useState("");
  const [selected, setSelected] = useState(["720p"]);
  const [uploading, setUploading] = useState(false);
  const [error, setError]       = useState("");
  const [progress, setProgress] = useState(0);

  const toggleRes = (r) =>
    setSelected((prev) =>
      prev.includes(r) ? prev.filter((x) => x !== r) : [...prev, r]
    );

  const submit = async (e) => {
    e.preventDefault();
    if (!file) return setError("Please select a video file.");
    if (!title.trim()) return setError("Please enter a title.");
    if (!selected.length) return setError("Select at least one resolution.");
    setError("");
    setUploading(true);
    setProgress(0);

    const fd = new FormData();
    fd.append("file", file);
    fd.append("title", title.trim());
    fd.append("description", desc.trim());
    fd.append("resolutions", selected.join(","));

    try {
      const res = await axios.post(`${API}/videos/upload`, fd, {
        headers: { "Content-Type": "multipart/form-data" },
        onUploadProgress: (e) => setProgress(Math.round((e.loaded / e.total) * 100)),
      });
      setFile(null); setTitle(""); setDesc(""); setSelected(["720p"]); setProgress(0);
      onUploaded(res.data);
    } catch (err) {
      setError(err?.response?.data?.detail || err.message || "Upload failed");
    } finally {
      setUploading(false);
    }
  };

  return (
    <div style={css.card}>
      <h2 style={css.h2}>Upload Video</h2>
      <form onSubmit={submit}>
        <div style={{ marginBottom: 14 }}>
          <label style={css.label}>Video File</label>
          <input
            type="file" accept="video/*"
            style={css.input}
            onChange={(e) => setFile(e.target.files[0])}
            disabled={uploading}
          />
        </div>
        <div style={{ marginBottom: 14 }}>
          <label style={css.label}>Title</label>
          <input style={css.input} value={title} onChange={(e) => setTitle(e.target.value)} disabled={uploading} />
        </div>
        <div style={{ marginBottom: 14 }}>
          <label style={css.label}>Description (optional)</label>
          <input style={css.input} value={desc} onChange={(e) => setDesc(e.target.value)} disabled={uploading} />
        </div>
        <div style={{ marginBottom: 18 }}>
          <label style={css.label}>Resolutions to encode &amp; store</label>
          <div style={css.checkRow}>
            {RESOLUTION_OPTIONS.map(({ value, label }) => (
              <label key={value} style={css.checkLabel}>
                <input
                  type="checkbox"
                  checked={selected.includes(value)}
                  onChange={() => toggleRes(value)}
                  disabled={uploading}
                />
                {label}
              </label>
            ))}
          </div>
        </div>
        {uploading && (
          <div style={{ marginBottom: 12 }}>
            <div style={{ fontSize: 12, color: "#aaa", marginBottom: 4 }}>
              {progress < 100 ? `Uploading… ${progress}%` : "Processing & storing on Autonomi network…"}
            </div>
            <div style={css.progress}><div style={css.progressBar(progress)} /></div>
          </div>
        )}
        {error && <div style={css.error}>{error}</div>}
        <button type="submit" style={{ ...css.btn, marginTop: 8 }} disabled={uploading}>
          {uploading ? "Uploading…" : "Upload & Store on Autonomi"}
        </button>
      </form>
    </div>
  );
}

// ── Video library ─────────────────────────────────────────────────────────────
function Library() {
  const [videos, setVideos]       = useState([]);
  const [loading, setLoading]     = useState(true);
  const [playing, setPlaying]     = useState(null);   // { videoId, resolution }
  const [detail, setDetail]       = useState(null);   // full VideoOut for expanded row

  const load = useCallback(async () => {
    try {
      const res = await axios.get(`${API}/videos`);
      setVideos(res.data);
    } catch { /* ignore */ }
    finally { setLoading(false); }
  }, []);

  useEffect(() => { load(); }, [load]);

  // Poll processing videos every 5 s
  useEffect(() => {
    const interval = setInterval(() => {
      if (videos.some((v) => v.status === "processing" || v.status === "pending")) {
        load();
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [videos, load]);

  const openDetail = async (videoId) => {
    if (detail?.id === videoId) return setDetail(null);
    const res = await axios.get(`${API}/videos/${videoId}`);
    setDetail(res.data);
  };

  const deleteVideo = async (videoId, e) => {
    e.stopPropagation();
    if (!window.confirm("Delete this video and all its stored segments?")) return;
    await axios.delete(`${API}/videos/${videoId}`);
    setVideos((prev) => prev.filter((v) => v.id !== videoId));
    if (detail?.id === videoId) setDetail(null);
    if (playing?.videoId === videoId) setPlaying(null);
  };

  if (loading) return <div style={css.empty}>Loading…</div>;
  if (!videos.length) return <div style={css.empty}>No videos yet. Upload one above.</div>;

  return (
    <div style={css.card}>
      <h2 style={css.h2}>Video Library</h2>
      <table style={css.table}>
        <thead>
          <tr>
            <th style={css.th}>Title</th>
            <th style={css.th}>Status</th>
            <th style={css.th}>Uploaded</th>
            <th style={css.th}>Actions</th>
          </tr>
        </thead>
        <tbody>
          {videos.map((v) => (
            <React.Fragment key={v.id}>
              <tr style={{ cursor: "pointer" }} onClick={() => openDetail(v.id)}>
                <td style={css.td}>{v.title}</td>
                <td style={css.td}><span style={css.badge(v.status)}>{v.status}</span></td>
                <td style={css.td} title={v.created_at}>{new Date(v.created_at).toLocaleDateString()}</td>
                <td style={css.td}>
                  <button style={{ ...css.btnDanger, marginLeft: 6 }} onClick={(e) => deleteVideo(v.id, e)}>Delete</button>
                </td>
              </tr>
              {detail?.id === v.id && (
                <tr>
                  <td colSpan={4} style={{ padding: "12px 10px 18px", background: "#0f1629", borderBottom: "1px solid #1f4068" }}>
                    {detail.variants.length === 0 ? (
                      <div style={{ color: "#888", fontSize: 13 }}>
                        {v.status === "processing" ? "Processing… check back shortly." : "No variants available."}
                      </div>
                    ) : (
                      <>
                        <div style={{ fontSize: 13, color: "#aaa", marginBottom: 8 }}>
                          Available resolutions:
                        </div>
                        <div style={css.resRow}>
                          {detail.variants.map((vt) => (
                            <button
                              key={vt.id}
                              style={css.resBtn(playing?.videoId === v.id && playing?.resolution === vt.resolution)}
                              onClick={() => setPlaying({ videoId: v.id, resolution: vt.resolution })}
                            >
                              {vt.resolution} ({vt.segment_count ?? "?"} segs)
                            </button>
                          ))}
                        </div>
                        {playing?.videoId === v.id && (
                          <VideoPlayer videoId={v.id} resolution={playing.resolution} />
                        )}
                      </>
                    )}
                  </td>
                </tr>
              )}
            </React.Fragment>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ── Root app ──────────────────────────────────────────────────────────────────
export default function App() {
  const [tab, setTab]     = useState("library");
  const [videos, setVideos] = useState([]);

  const handleUploaded = (video) => {
    setVideos((prev) => [video, ...prev]);
    setTab("library");
  };

  return (
    <div style={css.app}>
      <header style={css.header}>
        <span style={css.logo}>Autonomi Video</span>
        <nav style={css.nav}>
          <button style={css.navBtn(tab === "library")} onClick={() => setTab("library")}>Library</button>
          <button style={css.navBtn(tab === "upload")}  onClick={() => setTab("upload")}>Upload</button>
        </nav>
      </header>
      <main style={css.main}>
        {tab === "upload"  && <UploadPanel onUploaded={handleUploaded} />}
        {tab === "library" && <Library key={videos.length} />}
      </main>
    </div>
  );
}
