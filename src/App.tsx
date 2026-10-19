import { useEffect, useState } from "react";
import { motion } from "framer-motion";
import { Archive, Circle, Copy, FileText, Film, Image, Music, type LucideIcon } from "lucide-react";
import { listOperations, type Operation } from "./api";

const RADIUS = 130;

// Explicit map keeps the bundle small; add icons here as operations need them.
const ICONS: Record<string, LucideIcon> = { Archive, Copy, FileText, Film, Image, Music };

function iconFor(name: string): LucideIcon {
  return ICONS[name] ?? Circle;
}

export default function App() {
  const [ops, setOps] = useState<Operation[]>([]);
  const [active, setActive] = useState<string | null>(null);

  useEffect(() => {
    listOperations("application/octet-stream").then(setOps);
  }, []);

  return (
    <main className="flex h-full items-center justify-center">
      <div className="relative h-80 w-80">
        <motion.div
          className="absolute left-1/2 top-1/2 flex h-20 w-20 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full bg-zinc-800 text-sm"
          initial={{ scale: 0 }}
          animate={{ scale: 1 }}
        >
          {active ?? "Drop"}
        </motion.div>
        {ops.map((op, i) => {
          const angle = (i / ops.length) * 2 * Math.PI - Math.PI / 2;
          const Icon = iconFor(op.icon);
          return (
            <motion.button
              key={op.id}
              title={op.label}
              onHoverStart={() => setActive(op.label)}
              onHoverEnd={() => setActive(null)}
              className="absolute left-1/2 top-1/2 -ml-6 -mt-6 flex h-12 w-12 items-center justify-center rounded-full bg-indigo-600"
              initial={{ x: 0, y: 0, opacity: 0 }}
              animate={{ x: Math.cos(angle) * RADIUS, y: Math.sin(angle) * RADIUS, opacity: 1 }}
              whileHover={{ scale: 1.2 }}
              transition={{ type: "spring", delay: i * 0.04 }}
            >
              <Icon size={22} />
            </motion.button>
          );
        })}
      </div>
    </main>
  );
}
