$env:NSE_FO_LOCAL_IF="192.168.50.210"; cargo run -p box_scanner_live -- live
$env:NSE_FO_LOCAL_IF="192.168.50.210"; $env:RMTRADE_GATEWAY_API_KEY="abcdefghijklmnopqrstuvwxyz"; $env:RMTRADE_GATEWAY_PORT="48765"; cargo run -p box_scanner_live -- live
