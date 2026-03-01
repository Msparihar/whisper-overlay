import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { LiveWaveform } from "./components/ui/live-waveform";

function App() {
  const [active, setActive] = useState(false);
  const [transcribing, setTranscribing] = useState(false);

  useEffect(() => {
    const unlistenPress = listen("hotkey-press", () => {
      setActive(true);
      setTranscribing(false);
    });
    const unlistenRelease = listen("hotkey-release", () => {
      setActive(false);
      setTranscribing(true);
    });
    const unlistenDone = listen("transcribe-done", () => {
      setTranscribing(false);
    });

    return () => {
      unlistenPress.then((fn) => fn());
      unlistenRelease.then((fn) => fn());
      unlistenDone.then((fn) => fn());
    };
  }, []);

  return (
    <div className="w-full h-full" style={{ background: "transparent" }}>
      <div className="w-full h-full rounded-full bg-zinc-900 px-5 flex items-center">
        <div className="flex-1 min-w-0">
          <LiveWaveform
            active={active}
            processing={transcribing}
            mode="static"
            barColor="white"
            height={32}
            sensitivity={1.8}
            barWidth={2.5}
            barGap={1.5}
          />
        </div>
      </div>
    </div>
  );
}

export default App;
