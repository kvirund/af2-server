using System.Text;
using ExitGames.Client.Photon;

namespace PhotonProbe;

/// <summary>
/// Drives the real Photon client library from the Atom Fishing 2 install
/// against our own server, so the transport and the message envelope are
/// verified by the exact code the game runs.
///
/// It also replays the login handshake the game performs, which lets the
/// server be iterated on without launching the game, stealing focus or
/// clicking anything.
/// </summary>
public sealed class Probe : IPhotonPeerListener
{
    private const byte ParamMsgId = 1;
    private const byte ParamChecksum = 4;
    private const byte ParamCounter = 2;
    private const byte ParamPayload = 3;
    private const int ChecksumKey = 0x0A937F7F;

    private readonly PhotonPeer _peer;
    private int _counter;
    private bool _connected;
    private bool _failed;

    public Probe()
    {
        _peer = new PhotonPeer(this, ConnectionProtocol.Tcp)
        {
            DisconnectTimeout = 45000,
            CommandBufferSize = 100000,
            ChannelCount = 4,
            DebugOut = DebugLevel.ERROR
        };
    }

    public void DebugReturn(DebugLevel level, string message) => Console.WriteLine($"  [photon/{level}] {message}");

    public void OnStatusChanged(StatusCode statusCode)
    {
        Console.WriteLine($"  [status] {statusCode}");
        if (statusCode == StatusCode.Connect) _connected = true;
        if (statusCode is StatusCode.Disconnect or StatusCode.ExceptionOnConnect or StatusCode.Exception)
            _failed = true;
    }

    public void OnEvent(EventData eventData) => Report("event", eventData.Parameters);

    public void OnOperationResponse(OperationResponse response) => Report("response", response.Parameters);

    private static long LastFishId;
    private static int LastSpecies;
    private static int LastSizeClass;
    private static long CharacterId = 1;
    private static string SteamId = "76561190000000000";
    private static readonly List<long> ItemIds = [];

    private void LandFish(byte[] biteRequest)
    {
        for (int i = 0; i < 30; i++)
        {
            Send(1120, biteRequest);
            Pump(100);
        }
        Send(1130, [0, 0, 0, 0]);
        Pump(500);
    }

    private static long FirstOffer;

    private static void Report(string kind, Dictionary<byte, object> parameters)
    {
        object? id = parameters.TryGetValue(ParamMsgId, out object? v) ? v : null;
        byte[] payload = parameters.TryGetValue(ParamPayload, out object? p) && p is byte[] b ? b : [];
        Console.WriteLine($"  <- {kind} msg {id}, {payload.Length} bytes {Hex(payload, 48)}");
        if (id is int and 1130)
        {
            LastFishId = BitConverter.ToInt64(payload, 4);
            LastSpecies = BitConverter.ToInt32(payload, 12);
            LastSizeClass = BitConverter.ToInt32(payload, 20);
        }
        // Board: int taken, int most, 6 x short date, then offers starting with long id.
        if (id is int and 700 && payload.Length >= 28)
        {
            FirstOffer = BitConverter.ToInt64(payload, 20);
        }
        if (id is int and 110 && payload.Length >= 8)
        {
            CharacterId = BitConverter.ToInt64(payload, 0);
        }
        if (id is int and 180)
        {
            ReadItemIds(payload);
        }
    }

    /// <summary>Item records are 57 fixed bytes, then a length-prefixed XML string.</summary>
    private static void ReadItemIds(byte[] payload)
    {
        const int XmlLengthOffset = 57;
        for (int at = 0; at < payload.Length; at += XmlLengthOffset + 2 + BitConverter.ToInt16(payload, at + XmlLengthOffset))
        {
            ItemIds.Add(BitConverter.ToInt64(payload, at));
        }
    }

    private static string Hex(byte[] data, int limit)
    {
        var sb = new StringBuilder();
        int shown = Math.Min(data.Length, limit);
        for (int i = 0; i < shown; i++) sb.Append(data[i].ToString("X2")).Append(' ');
        if (data.Length > shown) sb.Append($"... (+{data.Length - shown})");
        return sb.ToString().TrimEnd();
    }

    private void Send(int msgId, byte[] payload)
    {
        _counter++;
        var parameters = new Dictionary<byte, object>
        {
            { ParamMsgId, msgId },
            { ParamChecksum, (_counter + msgId) ^ ChecksumKey },
            { ParamCounter, _counter },
            { ParamPayload, payload }
        };
        bool ok = _peer.OpCustom(1, parameters, true);
        Console.WriteLine($"  -> msg {msgId}, {payload.Length} bytes{(ok ? "" : "  (SEND FAILED)")}");
    }

    private void Pump(int milliseconds)
    {
        var deadline = DateTime.UtcNow.AddMilliseconds(milliseconds);
        while (DateTime.UtcNow < deadline)
        {
            _peer.Service();
            Thread.Sleep(20);
        }
    }

    /// <summary>The login the game sends in MainMenuScript.NMOJJJOBMII().</summary>
    private static byte[] BuildLogin()
    {
        using var ms = new MemoryStream();
        using var w = new BinaryWriter(ms, Encoding.UTF8);
        w.Write(1731L);                 // protocol marker
        WriteString(w, "");             // login
        WriteString(w, "");             // password
        WriteString(w, SteamId);          // steamId
        WriteString(w, "probe-device");  // deviceId
        WriteString(w, "Probe");         // persona
        w.Write((byte)1);                // isSteamLogin
        w.Write(0);                      // pin
        w.Flush();
        return ms.ToArray();
    }

    private static void WriteString(BinaryWriter w, string s)
    {
        byte[] bytes = Encoding.UTF8.GetBytes(s);
        w.Write((short)bytes.Length);
        w.Write(bytes);
    }

    public static int Main(string[] args)
    {
        string address = args.Length > 0 ? args[0] : "127.0.0.1:9201";
        const string appName = "AFCServer";
        if (args.Length > 1) SteamId = args[1];

        var probe = new Probe();
        Console.WriteLine($"Connecting to {address} as application \"{appName}\" ...");
        if (!probe._peer.Connect(address, appName))
        {
            Console.WriteLine("FAIL: Connect() refused to start");
            return 1;
        }

        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (!probe._connected && !probe._failed && DateTime.UtcNow < deadline)
        {
            probe._peer.Service();
            Thread.Sleep(20);
        }

        if (!probe._connected)
        {
            Console.WriteLine("FAIL: never reached StatusCode.Connect");
            return 1;
        }

        Console.WriteLine($"OK: connected, RTT {probe._peer.RoundTripTime} ms, server time {probe._peer.ServerTimeInMilliSeconds}");

        // Replay what the game does: login, then the three follow-ups the
        // client fires once the server answers 90 with "Ok".
        probe.Send(90, BuildLogin());
        probe.Pump(1500);

        probe.Send(105, [0, 0, 0, 0]);
        probe.Send(110, [0, 0, 0, 0]);
        probe.Pump(2000);

        // SelectShadowLogic.goToGame(): the "В игру" button sends the chosen
        // character id; the server answers 151 and the client loads Base.
        probe.Send(100, BitConverter.GetBytes(CharacterId));
        probe.Pump(2000);

        // LocButton.OnLocClick(): base 1, location 4, point 0. Answered by 158, 160.
        byte[] travel = new byte[12];
        BitConverter.TryWriteBytes(travel.AsSpan(0), 1);
        BitConverter.TryWriteBytes(travel.AsSpan(4), 4);
        probe.Send(138, travel);
        probe.Pump(1500);

        // UIVIPWindow.buyVIP(): int 0. Answered by 220 (1 bought, -1 short of ergs).
        probe.Send(220, [0, 0, 0, 0]);
        probe.Pump(500);

        // Board of the base: open the income and daily tabs and take the first offer of each.
        // Answered by 700, then 710 "ok=1", 680 and 670.
        foreach (int tab in new[] { 1000, 1100 })
        {
            probe.Send(700, BitConverter.GetBytes(tab));
            probe.Pump(500);
            probe.Send(710, BitConverter.GetBytes(FirstOffer));
            probe.Pump(500);
        }
        probe.Send(710, BitConverter.GetBytes(FirstOffer));
        probe.Pump(300);
        probe.Send(700, BitConverter.GetBytes(1110));
        probe.Pump(300);

        // PoplDrop bite request: rod 0, the sixth starter item as bait at offset 60. Repeat until the
        // random bite comes up, then land and keep the fish.
        byte[] biteRequest = new byte[95];
        BitConverter.TryWriteBytes(biteRequest.AsSpan(60), ItemIds[5]);
        probe.LandFish(biteRequest);
        probe.Send(1135, BitConverter.GetBytes(LastFishId));
        probe.Pump(500);
        long keptFirst = LastFishId;
        int soldSpecies = LastSpecies;
        int soldSizeClass = LastSizeClass;

        // Let one go on landing, keep another and move it to the spare keepnet.
        probe.LandFish(biteRequest);
        probe.Send(286, BitConverter.GetBytes(LastFishId));
        probe.Pump(300);
        probe.LandFish(biteRequest);
        probe.Send(1135, BitConverter.GetBytes(LastFishId));
        probe.Pump(300);
        probe.Send(176, BitConverter.GetBytes(LastFishId));
        probe.Pump(300);

        // Talk to an NPC, ask the prices, sell the first fish's kind.
        byte[] npc = new byte[16];
        BitConverter.TryWriteBytes(npc.AsSpan(0), 3L);
        probe.Send(60, npc);
        probe.Pump(300);
        probe.Send(1205, BitConverter.GetBytes(3));
        probe.Pump(300);
        byte[] sell = new byte[12];
        BitConverter.TryWriteBytes(sell.AsSpan(0), 3);
        BitConverter.TryWriteBytes(sell.AsSpan(4), soldSpecies);
        BitConverter.TryWriteBytes(sell.AsSpan(8), soldSizeClass);
        probe.Send(1210, sell);
        probe.Pump(300);
        Console.WriteLine($"  kept {keptFirst}, sold species {soldSpecies} class {soldSizeClass}");

        // The shop request that used to leave the screen black.
        probe.Send(1309, [0xFF, 0xFF, 0xFF, 0xFF, 2, 0, 0x31, 0x30]);
        probe.Pump(500);

        // Buy two of bait 1010 from shop -1.
        byte[] buy = new byte[12];
        BitConverter.TryWriteBytes(buy.AsSpan(0), -1);
        BitConverter.TryWriteBytes(buy.AsSpan(4), 1010);
        BitConverter.TryWriteBytes(buy.AsSpan(8), 2);
        probe.Send(1310, buy);
        probe.Pump(500);

        // Drop the starter bait in the backpack, then put it back on the rod.
        byte[] move = new byte[16];
        BitConverter.TryWriteBytes(move.AsSpan(0), ItemIds[5]);
        BitConverter.TryWriteBytes(move.AsSpan(8), -1);
        BitConverter.TryWriteBytes(move.AsSpan(12), 100);
        probe.Send(182, move);
        probe.Pump(300);
        BitConverter.TryWriteBytes(move.AsSpan(8), 100);
        BitConverter.TryWriteBytes(move.AsSpan(12), 20);
        probe.Send(182, move);
        probe.Pump(300);

        // Exit to Pustosh on the map, arrive on the invented Pra, fish there.
        byte[] travelTo = new byte[12];
        BitConverter.TryWriteBytes(travelTo.AsSpan(0), 3);
        BitConverter.TryWriteBytes(travelTo.AsSpan(4), 31);
        probe.Send(134, travelTo);
        probe.Pump(300);
        BitConverter.TryWriteBytes(travelTo.AsSpan(0), 9);
        BitConverter.TryWriteBytes(travelTo.AsSpan(4), 91);
        probe.Send(137, travelTo);
        probe.Pump(300);
        for (int i = 0; i < 30; i++)
        {
            probe.Send(1120, biteRequest);
            probe.Pump(100);
        }

        // Panel windows, warehouse, buying bag cells and moving part of a stack.
        using (var text = new MemoryStream())
        using (var writer = new BinaryWriter(text))
        {
            WriteString(writer, "?");
            writer.Flush();
            probe.Send(32, text.ToArray());
        }
        foreach (int tab in new[] { 1150, 30, 101, 40, 130, 1401, 330, 450 })
        {
            probe.Send(tab, BitConverter.GetBytes(0));
        }
        byte[] station = new byte[8];
        BitConverter.TryWriteBytes(station.AsSpan(4), 1000);
        probe.Send(1700, station);
        probe.Send(1230, BitConverter.GetBytes(1));
        probe.Send(210, BitConverter.GetBytes(1));
        byte[] buyCells = new byte[8];
        BitConverter.TryWriteBytes(buyCells.AsSpan(0), 1);
        BitConverter.TryWriteBytes(buyCells.AsSpan(4), 1);
        probe.Send(215, buyCells);
        byte[] split = new byte[20];
        BitConverter.TryWriteBytes(split.AsSpan(0), ItemIds[5]);
        BitConverter.TryWriteBytes(split.AsSpan(8), 1);
        BitConverter.TryWriteBytes(split.AsSpan(12), 52);
        BitConverter.TryWriteBytes(split.AsSpan(16), 0);
        probe.Send(200, split);
        probe.Pump(800);

        // An id the server has no handler for, to exercise the UNANSWERED log.
        probe.Send(4242, [1, 2, 3]);
        probe.Pump(800);

        probe._peer.Disconnect();
        probe._peer.Service();
        Console.WriteLine("Done.");
        return 0;
    }
}
