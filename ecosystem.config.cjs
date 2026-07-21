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
      max_memory_restart: "700M", // hard ceiling; compression of huge images is memory-hungry
      env: {
        PORT: "2500",
        SW3_DATA: "./data",
        TRUST_PROXY: "1", // behind nginx: rate-limit by X-Forwarded-For client IP
      },
    },
  ],
};
