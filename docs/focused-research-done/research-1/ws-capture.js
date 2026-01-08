const WebSocket = require('ws');
const fs = require('fs');

const LIVE_DATA_WS = 'wss://ws-live-data.polymarket.com/';

const messages = [];
const startTime = Date.now();
const CAPTURE_DURATION_MS = 30000; // 30 seconds

console.log('Connecting to Polymarket Live Data WebSocket...');

const ws = new WebSocket(LIVE_DATA_WS);

ws.on('open', () => {
    console.log('Connected! Subscribing...');

    const subscribeMsg = JSON.stringify({
        "action": "subscribe",
        "subscriptions": [
            {
                "topic": "activity",
                "type": "orders_matched",
                "filters": "{\"event_slug\":\"btc-updown-15m-1767735000\"}"
            },
            {
                "topic": "comments",
                "type": "*",
                "filters": "{\"parentEntityID\":10192,\"parentEntityType\":\"Series\"}"
            },
            {
                "topic": "crypto_prices_chainlink",
                "type": "update",
                "filters": "{\"symbol\":\"btc/usd\"}"
            }
        ]
    });

    ws.send(subscribeMsg);
    console.log(`Capturing for ${CAPTURE_DURATION_MS / 1000} seconds...`);
});

ws.on('message', (data) => {
    const msg = data.toString();
    console.log(msg);
    messages.push({
        timestamp: Date.now(),
        raw: msg
    });
});

ws.on('error', (err) => {
    console.error('Error:', err.message);
});

ws.on('close', () => {
    console.log('Closed');
    saveMessages();
});

setTimeout(() => {
    console.log(`\nDone. ${messages.length} messages.`);
    ws.close();
}, CAPTURE_DURATION_MS);

function saveMessages() {
    const outputFile = `/Users/pontidev/Developer/poly-bot/focused-research/research-1/ws-live-capture-${Date.now()}.json`;
    fs.writeFileSync(outputFile, JSON.stringify(messages, null, 2));
    console.log(`Saved to: ${outputFile}`);
    process.exit(0);
}
