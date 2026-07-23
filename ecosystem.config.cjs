// PM2 config for running SW3 Images on a Raspberry Pi (or any Linux box):
//   cargo build --release
//   pm2 start ecosystem.config.cjs
//   pm2 save && pm2 startup   # survive reboots
module.exports = {
  apps: [
    {
      name: "sw3-images",
      script: "./target/release/sw3-images",
      cwd: __dirname,
      instances: 1,
      exec_mode: "fork",
      autorestart: true,
      // 3 GB ceiling on an 8 GB Raspberry Pi — comfortable headroom for one
      // compression at a time (SW3_CONCURRENCY=1) even on a large multi-
      // megapixel photo, while leaving 5 GB for the OS and sibling services.
      // If the process ever grows past this, something has genuinely gone
      // wrong and a restart is the safe response.
      max_memory_restart: "3072M",
      env: {
        PORT: "2500",
        SW3_DATA: "./data",
        TRUST_PROXY: "1", // behind nginx: rate-limit by X-Forwarded-For client IP
        // Override to allow parallel compressions (default: 1, safest).
        // SW3_CONCURRENCY: "2",
      },
    },
  ],
};
