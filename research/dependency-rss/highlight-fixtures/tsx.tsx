import { useState } from "react";

interface Turn {
  id: string;
  role: "user" | "assistant";
  text: string;
}

export function TurnList({ initial }: { initial: Turn[] }) {
  const [turns, setTurns] = useState<Turn[]>(initial);
  const [draft, setDraft] = useState("");

  function send() {
    const text = draft.trim();
    if (text === "") {
      return;
    }
    setTurns((prev) => [...prev, { id: crypto.randomUUID(), role: "user", text }]);
    setDraft("");
  }

  return (
    <div className="turns">
      <ul>
        {turns.map((t) => (
          <li key={t.id} className={t.role}>
            <span className="role">{t.role}</span>
            <p>{t.text}</p>
          </li>
        ))}
      </ul>
      <div className="composer">
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="Ask anything…"
        />
        <button onClick={send} disabled={draft.trim() === ""}>
          Send
        </button>
      </div>
    </div>
  );
}
