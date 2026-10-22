# Regenerates the audio fixtures used by the test-suite.
# Requires ffmpeg on PATH (built with libmp3lame and libvorbis).
# The generated files are committed, so running this is only needed when fixtures change.

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

function ff {
    & ffmpeg -hide_banner -loglevel error -y @args
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed: $args" }
}

$sine44 = 'sine=frequency=440:sample_rate=44100:duration=2'
$sine48 = 'sine=frequency=440:sample_rate=48000:duration=2'

# 2 s, 440 Hz stereo sine in every supported codec.
ff -f lavfi -i $sine44 -ac 2 '-c:a' pcm_s16le sine_44k_stereo.wav
ff -f lavfi -i $sine44 -ac 2 '-c:a' libmp3lame '-b:a' 320k sine_44k_cbr.mp3
ff -f lavfi -i $sine44 -ac 2 '-c:a' libmp3lame '-q:a' 4 sine_44k_vbr.mp3
ff -f lavfi -i $sine44 -ac 2 '-c:a' flac sine_44k_stereo.flac
ff -f lavfi -i $sine44 -ac 2 '-c:a' libvorbis '-q:a' 3 sine_44k_stereo.ogg
ff -f lavfi -i $sine44 -ac 2 '-c:a' aac '-b:a' 128k sine_44k_stereo.m4a

# Variants: device-rate stereo and mono sources.
ff -f lavfi -i $sine48 -ac 2 '-c:a' flac sine_48k_stereo.flac
ff -f lavfi -i $sine44 -ac 1 '-c:a' libmp3lame '-b:a' 128k sine_44k_mono.mp3

# Tagged files (ID3v2 + Vorbis comment) including ReplayGain.
$tags = @(
    '-metadata', 'title=Test Title',
    '-metadata', 'artist=Test Artist',
    '-metadata', 'album=Test Album',
    '-metadata', 'date=1999',
    '-metadata', 'genre=Rock',
    '-metadata', 'REPLAYGAIN_TRACK_GAIN=-6.50 dB',
    '-metadata', 'REPLAYGAIN_TRACK_PEAK=0.500000'
)
ff -f lavfi -i $sine44 -ac 2 '-c:a' libmp3lame '-b:a' 128k -id3v2_version 3 @tags tagged.mp3
ff -f lavfi -i $sine44 -ac 2 '-c:a' flac @tags tagged.flac

# Gapless pair: a 48 kHz stereo chirp, plus the same chirp split at an odd sample offset.
# FLAC is lossless, so decoding the two halves back to back must reproduce the full file exactly.
$chirp = "aevalsrc='0.5*sin(2*PI*(200*t+400*t*t))|0.5*sin(2*PI*(300*t+300*t*t))':s=48000:d=1.5"
ff -f lavfi -i $chirp '-c:a' flac -sample_fmt s16 chirp_full.flac
ff -f lavfi -i $chirp -af 'atrim=end_sample=33331' '-c:a' flac -sample_fmt s16 chirp_part1.flac
ff -f lavfi -i $chirp -af 'atrim=start_sample=33331,asetpts=PTS-STARTPTS' '-c:a' flac -sample_fmt s16 chirp_part2.flac

Get-ChildItem -File | Where-Object Extension -ne '.ps1' | Format-Table Name, Length
