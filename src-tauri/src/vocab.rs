/// Tech terms used as Deepgram keyterms and as Whisper prompt vocabulary.
/// Synced from STT-Go main.go techTerms list.
pub const TECH_TERMS: &[&str] = &[
    "OpenAI", "Claude", "Anthropic", "GPT-4", "Whisper", "Deepgram",
    "GitHub", "GitLab", "WinSCP", "WSL", "Ubuntu", "Docker", "Kubernetes",
    "Traefik", "Dokploy", "Cloudflare", "Vercel", "Supabase", "PostgreSQL", "Redis",
    "TypeScript", "JavaScript", "Golang", "Next.js", "Svelte",
    "Zustand", "React Query", "Tailwind CSS", "Prisma", "Drizzle", "tRPC",
    "GraphQL", "WebSocket", "gRPC",
    "bun", "pnpm", "uv", "cargo",
    "CLAUDE.md", ".env.local", "tsconfig", "go.mod",
    "Direct2D", "Direct3D", "Win32", "GDI", "syscall", "vtable", "HWND",
    "nginx", "systemd", "htop", "tmux", "neovim",
    "ESLint", "Prettier", "Biome",
    "Figma", "Linear",
    "Haiku", "Claude Code", "stt-go", "Manish", "Hostinger", "PowerShell",
    "systray", "energye", "waveIn", "zshrc", "bashrc",
    "Anthropic API", "MCP", "subagent", "worktree",
];

/// Build the full Whisper prompt string from tech terms + extra common terms.
pub fn whisper_prompt() -> String {
    let mut parts: Vec<&str> = TECH_TERMS.to_vec();
    let extras = [
        "SSH", "API", "REST", "JSON", "YAML", "TOML", "npm", "yarn", "pip",
        ".env", "package.json", "COM", "DLL", "cron", "vim", "PR", "CI/CD",
        "linting", "Notion", "Slack", "Discord", "setx", "hooks", "worktree",
    ];
    parts.extend_from_slice(&extras);
    parts.join(", ")
}
