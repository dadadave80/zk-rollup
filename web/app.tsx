import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { privateKeyToAccount } from "viem/accounts";
import { concat, hexToBytes, keccak256, numberToBytes, type Hex } from "viem";

const SEQUENCER = "http://localhost:7001";

const KEYS: Record<string, Hex> = {
  alice: "0x0101010101010101010101010101010101010101010101010101010101010101",
  bob: "0x0202020202020202020202020202020202020202020202020202020202020202",
};

const ALICE = privateKeyToAccount(KEYS.alice);
const BOB = privateKeyToAccount(KEYS.bob);

interface Info {
  canonical_root: string;
  batch_count: number;
  mempool_size: number;
  program_vkey: string;
  proof_mode: string;
  rollup_address: string;
}

interface MempoolEntry {
  from: string;
  to: string;
  amount: number;
  nonce: number;
}

interface Account {
  address: string;
  balance: number;
  nonce: number;
}

interface BatchSettled {
  batch_number: number;
  txs: number;
  prev_root: string;
  new_root: string;
  batch_hash: string;
  l1_tx_hash: string;
  l1_block: number;
  gas_used: number;
}

type Toast = { kind: "info" | "success" | "error"; msg: string } | null;

function shortHash(h: string, head = 8, tail = 6) {
  if (!h.startsWith("0x") || h.length <= head + tail + 2) return h;
  return `${h.slice(0, head + 2)}…${h.slice(-tail)}`;
}

function signingHash(from: Hex, to: Hex, amount: bigint, nonce: bigint): Hex {
  return keccak256(
    concat([
      hexToBytes(from),
      hexToBytes(to),
      numberToBytes(amount, { size: 8 }),
      numberToBytes(nonce, { size: 8 }),
    ]),
  );
}

function App() {
  const [info, setInfo] = useState<Info | null>(null);
  const [up, setUp] = useState<boolean>(false);
  const [mempool, setMempool] = useState<MempoolEntry[]>([]);
  const [accounts, setAccounts] = useState<Record<string, Account>>({});
  const [batches, setBatches] = useState<BatchSettled[]>([]);
  const [batching, setBatching] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [toast, setToast] = useState<Toast>(null);
  const lastBatchCountRef = useRef<number>(-1);

  const showToast = useCallback((t: Toast, ms = 4500) => {
    setToast(t);
    if (t) setTimeout(() => setToast(null), ms);
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [iRes, mRes, aRes, bRes] = await Promise.all([
        fetch(`${SEQUENCER}/info`),
        fetch(`${SEQUENCER}/mempool`),
        fetch(`${SEQUENCER}/state/${ALICE.address.toLowerCase()}`),
        fetch(`${SEQUENCER}/state/${BOB.address.toLowerCase()}`),
      ]);
      const i = (await iRes.json()) as Info;
      setInfo(i);
      setUp(true);
      setMempool((await mRes.json()) as MempoolEntry[]);
      const aliceAcc = (await aRes.json()) as Account;
      const bobAcc = (await bRes.json()) as Account;
      setAccounts({
        [ALICE.address.toLowerCase()]: aliceAcc,
        [BOB.address.toLowerCase()]: bobAcc,
      });
    } catch {
      setUp(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 1500);
    return () => clearInterval(id);
  }, [refresh]);

  // Detect batch_count increases and append to local batch history.
  useEffect(() => {
    if (!info) return;
    if (lastBatchCountRef.current === -1) {
      lastBatchCountRef.current = info.batch_count;
    }
  }, [info]);

  const submitTransfer = useCallback(
    async (sender: "alice" | "bob", amount: bigint) => {
      const fromAcc = sender === "alice" ? ALICE : BOB;
      const toAcc = sender === "alice" ? BOB : ALICE;
      const fromAddr = fromAcc.address.toLowerCase();
      const acc = accounts[fromAddr];
      if (!acc) {
        showToast({ kind: "error", msg: "sequencer hasn't reported state for this account yet" });
        return;
      }
      setSubmitting(true);
      try {
        // Use the sender's current nonce + count of their own pending mempool txs.
        const pending = mempool.filter((t) => t.from.toLowerCase() === fromAddr).length;
        const nonce = BigInt(acc.nonce + pending);
        const hash = signingHash(fromAcc.address, toAcc.address, amount, nonce);
        const signature = await fromAcc.sign({ hash });
        const r = await fetch(`${SEQUENCER}/tx`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            from: fromAcc.address,
            to: toAcc.address,
            amount: Number(amount),
            nonce: Number(nonce),
            signature,
          }),
        });
        if (!r.ok) {
          const err = await r.text();
          showToast({ kind: "error", msg: `tx rejected: ${err}` });
          return;
        }
        showToast({
          kind: "success",
          msg: `submitted ${sender}→${sender === "alice" ? "bob" : "alice"} ${amount} (nonce ${nonce})`,
        });
        refresh();
      } catch (e) {
        showToast({ kind: "error", msg: e instanceof Error ? e.message : String(e) });
      } finally {
        setSubmitting(false);
      }
    },
    [accounts, mempool, refresh, showToast],
  );

  const triggerBatch = useCallback(async () => {
    if (mempool.length === 0) {
      showToast({ kind: "error", msg: "mempool is empty" });
      return;
    }
    setBatching(true);
    showToast(
      {
        kind: "info",
        msg:
          info?.proof_mode === "groth16"
            ? "proving with Groth16 — this can take several minutes…"
            : "proving with mock prover…",
      },
      60_000,
    );
    try {
      const r = await fetch(`${SEQUENCER}/batch`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      });
      if (!r.ok) {
        const err = await r.text();
        showToast({ kind: "error", msg: `/batch failed: ${err}` });
        return;
      }
      const b = (await r.json()) as BatchSettled;
      setBatches((prev) => [b, ...prev].slice(0, 20));
      showToast({
        kind: "success",
        msg: `batch #${b.batch_number} settled in tx ${shortHash(b.l1_tx_hash)} (${b.gas_used.toLocaleString()} gas)`,
      });
      refresh();
    } catch (e) {
      showToast({ kind: "error", msg: e instanceof Error ? e.message : String(e) });
    } finally {
      setBatching(false);
    }
  }, [info?.proof_mode, mempool.length, refresh, showToast]);

  return (
    <div className="container">
      <header className="header">
        <h1>
          <span className={`status-dot ${up ? "up" : "down"}`}></span>
          SP1 zk-rollup
          <small>{up ? "sequencer up" : "sequencer down"}</small>
        </h1>
        <div className="muted mono" style={{ fontSize: 11 }}>
          mode: <span className="accent">{info?.proof_mode ?? "—"}</span> · batch{" "}
          <span className="accent">#{info?.batch_count ?? "—"}</span> · pending{" "}
          <span className="accent">{info?.mempool_size ?? "—"}</span>
        </div>
      </header>

      <div className="grid">
        <section className="panel full">
          <h2>state</h2>
          <div className="kv">
            <span className="k">canonical root</span>
            <span className="v">{info?.canonical_root ?? "—"}</span>
            <span className="k">program vkey</span>
            <span className="v">{info?.program_vkey ?? "—"}</span>
            <span className="k">rollup contract</span>
            <span className="v">{info?.rollup_address ?? "—"}</span>
          </div>
        </section>

        <section className="panel">
          <h2>accounts</h2>
          <table>
            <thead>
              <tr>
                <th>name</th>
                <th>balance</th>
                <th>nonce</th>
                <th>address</th>
              </tr>
            </thead>
            <tbody>
              {[
                { name: "alice", acc: accounts[ALICE.address.toLowerCase()] },
                { name: "bob", acc: accounts[BOB.address.toLowerCase()] },
              ].map(({ name, acc }) => (
                <tr key={name}>
                  <td>{name}</td>
                  <td>{acc?.balance ?? "—"}</td>
                  <td>{acc?.nonce ?? "—"}</td>
                  <td className="muted">
                    {acc ? shortHash(acc.address, 6, 4) : "—"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>

        <section className="panel">
          <h2>submit transfer</h2>
          <TransferForm onSubmit={submitTransfer} disabled={submitting || !up} />
        </section>

        <section className="panel">
          <div className="toolbar">
            <h2 style={{ margin: 0 }}>mempool ({mempool.length})</h2>
            <button
              onClick={triggerBatch}
              disabled={batching || mempool.length === 0 || !up}
              className="warm"
            >
              {batching ? "proving…" : "build batch + settle"}
            </button>
          </div>
          {mempool.length === 0 ? (
            <div className="empty">empty</div>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>#</th>
                  <th>from</th>
                  <th>to</th>
                  <th>amount</th>
                  <th>nonce</th>
                </tr>
              </thead>
              <tbody>
                {mempool.map((t, i) => (
                  <tr key={`${t.from}-${t.nonce}`}>
                    <td className="muted">{i + 1}</td>
                    <td>{t.from.toLowerCase() === ALICE.address.toLowerCase() ? "alice" : "bob"}</td>
                    <td>{t.to.toLowerCase() === ALICE.address.toLowerCase() ? "alice" : "bob"}</td>
                    <td>{t.amount}</td>
                    <td>{t.nonce}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <section className="panel">
          <h2>settled batches (this session)</h2>
          {batches.length === 0 ? (
            <div className="empty">no batches yet — submit a transfer and click build batch</div>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>#</th>
                  <th>txs</th>
                  <th>new root</th>
                  <th>l1 block</th>
                  <th>gas</th>
                </tr>
              </thead>
              <tbody>
                {batches.map((b) => (
                  <tr key={b.l1_tx_hash} className="row-flash">
                    <td>#{b.batch_number}</td>
                    <td>{b.txs}</td>
                    <td className="mono">{shortHash(b.new_root)}</td>
                    <td className="muted">{b.l1_block}</td>
                    <td>{b.gas_used.toLocaleString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>

      {toast && (
        <div className={`toast ${toast.kind}`}>
          {toast.msg}
        </div>
      )}
    </div>
  );
}

function TransferForm({
  onSubmit,
  disabled,
}: {
  onSubmit: (sender: "alice" | "bob", amount: bigint) => void;
  disabled: boolean;
}) {
  const [sender, setSender] = useState<"alice" | "bob">("alice");
  const [amount, setAmount] = useState("100");

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const n = BigInt(amount || "0");
    if (n <= 0n) return;
    onSubmit(sender, n);
  };

  return (
    <form onSubmit={submit}>
      <label>
        sender
        <select value={sender} onChange={(e) => setSender(e.target.value as "alice" | "bob")}>
          <option value="alice">alice</option>
          <option value="bob">bob</option>
        </select>
      </label>
      <label>
        amount
        <input
          type="number"
          min="1"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          style={{ width: 100 }}
        />
      </label>
      <button type="submit" disabled={disabled}>
        sign + send
      </button>
    </form>
  );
}

const root = createRoot(document.getElementById("root")!);
root.render(<App />);
