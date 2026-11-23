# Camera

The viewfinders, the pixel cam and the two rolls. The camera pipeline is in [Camera](../firmware/camera.md), decoding and playback in [Media](../firmware/media.md). The `CAMERA` menu has the same four entries in every theme: photo, video, pictures, videos. The window that scans a code, for the OpenAI key or a tunnel's config, has its own viewfinder ([Interface](interface.md#the-openai-key)).

## Viewfinders

| | Photo | Video |
|---|---|---|
| Wheel | down selects the front camera, up the rear | the same, except while recording |
| Button | takes a photo; on the review of the photo just taken, back to the viewfinder | starts or stops a recording |
| Touch on the robot | with the camera tool on, takes a photo for the robot alone, saved nowhere, the viewfinder stopped on the frame it showed until the conversation opens with the photo as it comes ([Chat](chat.md#photos)) | nothing |
| Sensor mode | 1632x1224 at 60 fps; 3264x2448 for the photo | 1280x720 at 60 fps |
| Picture | the middle square, 480x480, under the bar | the whole frame upright, 305x543, between dark bars, in every theme, Terminal's status line's place included, as no status line shows over a viewfinder; a red disc and the elapsed time while recording |
| File | `camera/pictures/STAMP_original.jpg`, 2448x2448 JPEG, finest tables, a burst of two to eight frames merged ([Camera](../firmware/camera.md#photo)) | `camera/videos/STAMP.mp4`: H.264 High, 16 Mbit/s, one key frame per second, AAC 96 kbit/s |

- Opening a viewfinder turns the camera out to the side last selected (rear after boot). Leaving it (a swipe home or back, or sleep) stops a recording, keeps the file and turns the camera in.
- The accelerometer at the click sets the rotation: a photo is saved upright relative to the scene; a recording stores its rotation in the track matrix. The front camera is mirrored in the viewfinder and the review only. The recording mark and the camera's messages rotate with the device; the picture does not.
- **Review.** At the click the photo viewfinder stops on the frame it showed; the camera takes the burst, and its stream rests (`View::Pause`) until the review ends or a touch on the robot asks for a photo for it. Once the burst is developed, the photo as saved unfolds over that frame at the viewfinder's size; once the pixel cam has drawn its art, the art unfolds over the photo (`gfx::store`). Of the front camera both show mirrored, as the frame did (`camera::Sight`); the files are not. Only the pictures of the photo whose burst was taken at that click show (`Report::taken`). A click or a swipe from the left goes back to the viewfinder, ready for the next photo; the photos taken meanwhile go on developing. Leaving the viewfinder lets the review go.
- `SAVED` with the file name for a recording, `PHOTO FAILED` (which ends a review), `VIDEO FAILED` and `STORAGE FULL` (a recording needs 64 MiB free) show for a second at the bottom. `CAMERA TOO WARM`, `CAMERA FAILED` and `BATTERY TOO LOW` (off the cable under 10 % the camera does not power up, [Battery](../firmware/battery.md#camera-reserve)) stay until the viewfinder closes; a video call's camera, which runs with no viewfinder, tells them on the call's screen for a second ([Interface](interface.md#calls)). While recording, the device does not sleep on its own; when a recording stops on its own is in [Camera](../firmware/camera.md#recording).

## Pixel cam

Every saved photo is sent to `gpt-image-2.5-flare`, which redraws it as pixel art. The result is saved next to the photo as `STAMP_pixel.jpg`, 1024x1024, unchanged from the model's output. Sending starts when the photo is saved, whatever screen shows. A second photo taken in the same second is named `STAMP_1`.

**Request** (`r1_funcore::openai::image`, sent by `crates/impshell/src/openai/image.rs`): a multipart form posted to `/v1/images/edits` with the key as bearer token. Fields: `model`, `prompt` (`crates/funcore/assets/magic/prompt.txt`), `size` `1024x1024`, `quality` `medium`, `output_format` `jpeg`, `output_compression` `90`, and `image[]`, the photo at 1024x1024 ([Camera](../firmware/camera.md#photo)).

The prompt asks for the scene as a 16-bit-era adventure game artist would draw it. It must keep every main object with its position and material, the place, the time of day and the light, and add nothing. Style: bold shapes, square pixels as if drawn at about 128x128, flat colour, dark outlines, dithering only for gradients, no text, no frame. Palette: 16 to 24 colours, warm, cozy and vibrant, keeping the scene as bright or dark as it is.

**Response**: JSON with the picture in base64 at `data[0].b64_json`. Token counts are read by full path (`usage.input_tokens_details.text_tokens`, `usage.input_tokens_details.image_tokens`, `usage.output_tokens`), because `text_tokens` and `image_tokens` appear twice. A picture that does not start as a JPEG is discarded. A photo costs 426 text and 1024 image tokens in, 439 out: 2.35 cents ([OpenAI](openai.md#prices)).

**Threads** (`crates/impshell/src/magic.rs`): three `magic` threads at the lowest priority, idle until a photo waits.

1. The darkroom saves the photo, then makes the 1024x1024 copy and hands it over. The copy is kept as `.r1metal/magic/STAMP.jpg` until the pixel art is saved.
2. A photo just taken is sent whatever screen shows. A photo found waiting is sent only while the photo viewfinder or a roll shows.
3. Before the network is requested, the thread checks that the photo exists and has no pixel art, or that new art was requested (`magic::is_needless`); that the data partition has room for the result (`magic::has_room`); and that a key exists.
4. The thread takes a network hold (`Hold`), or keeps the previous photo's. Photos sent together or in sequence share one stay (`Stay`) and its open connections ([OpenAI](openai.md#account)). It sends the form and waits. The model works for about 11 s before it answers (`magic::DRAW_TIME_NS`), so the connection allows 120 s per read.
5. Once the response is read, the thread releases the network before saving anything, unless another photo is due. It parses the response, books the cost in the ledger, saves the art, checks that the photo still exists (`magic::is_gone`), deleting the art again if not, and deletes the copy.

At most three photos are with the model at once (`magic::MAX_PARALLEL`): the service's lowest tier accepts 20 pictures per minute, and at 11 s each, three stay below that.

| Result | Then | Screen shows |
|---|---|---|
| Art saved | done | the progress row fills, the robot jumps with a grin |
| Photo deleted or already has art | the copy is deleted | nothing |
| Photo deleted while the model drew it | the art just saved and the copy are deleted, and a roll that listed the art meanwhile lists anew (`magic::Outcome::Withdrawn`); the tokens are booked | nothing |
| Refused for good (`moderation_blocked`), or status 200 without a picture | not sent again, the copy is deleted; the tokens a 200 counted are booked | `PIXELS REFUSED` |
| No network | nothing more is sent until a photo screen opens again | `NO NETWORK` |
| No key | the same | `NO OPENAI KEY` |
| Any other failure | this photo waits until a photo screen opens or new art is requested; other photos continue | `NO PIXELS` |

`magic::State::take_outcome` takes how the redraw ended, kept, not needed or the `Fault` it came to. A response without art is refused when the service blocks the photo or answered 200 without it (`image::Error::to_fault`); a network lost mid-request, or a 429 or 5xx, counts as "any other failure". A redraw that is done (`magic::is_done`: saved, not needed, or refused) is not retried, and its copy is deleted.

Opening a photo screen retries every copy in `.r1metal/magic`, which also picks up photos taken before a restart. Sleep ends the stay and any redraw in it; that photo waits. The log has one line per redraw: file and size, tokens and cost, and the milliseconds until the network, the service, the form sent, the response header, the response read and the file saved. What turning the radio on adds is in [Network](../firmware/network.md#typical-timings).

**Progress row.** While a burst is developed or a photo is redrawn, a row of ten blocks shows in the bar of the photo viewfinder, its review and the rolls. It ends one gap left of the robot's seat, at the height of a screen title. The blocks are the brightness screen's blocks, dim until lit, growing from their centre. The main loop wakes only for the three timed blocks and draws only while a block animates; on other screens nothing is drawn or woken. With several photos in progress, the progress row shows the redrawing furthest along, or the development where none is redrawn. A result message shows for a second where the camera's messages show, if a photo screen is open.

| Block | Lit when |
|---|---|
| 1 | the burst is merged |
| 2 | the photo is saved |
| 3 | joined a network |
| 4 | the service is reached |
| 5 | the photo is sent |
| 6, 7, 8 | a quarter, half and three quarters of 11 s later |
| 9 | the response begins |
| 10 | the art is saved; the full row stays for a second, then disappears |

## Rolls

`PICTURES` (`camera/pictures`) and `VIDEOS` (`camera/videos`), newest first. Files, pictures and the decoder are held only while a roll shows.

A photo and its pixel art count as one item. A roll opens on the pixel art. A touch on the robot in the bar switches to the original photos, another touch back to the art. A photo without art shows the original either way; while its art is on the way, the bar shows `WAITING`. When the art is saved the roll reloads: in the grid the art unfolds over the photo (in the Terminal theme and on the drum it is cut in, with its stamped icons), and a photo shown large is replaced by its art.

**Grid.** Two thumbnails across and three down under the bar, each 228x173, 187x173 in Rail's pane, 8 px apart ([Style guide](style-guide.md#8-grids-and-lists)). The UI owns the selection and draws. It sends the `gallery` thread the page shown, the number and size of its slots (`RollRequest::places`, `RollRequest::place_size`), how many items beside the page a drum shows (`RollRequest::margin`), which item is open, and the room it stands in (`RollRequest::room`); the `gallery` thread loads the thumbnails and large pictures. While home shows, the `gallery` thread counts the photos, the photos taken today and the videos (`Gallery::set_home`, `Job::Count`).

**Drum.** In the Dial theme a roll is a drum (`ui/picture/drum.rs`). The selected item sits in a band across the middle of the area under the bar, as tall as that area divided by φ. On each side, two rings show the items before and after it: the first ring is the space beside the band, less two gaps, divided by φ; the second is the first divided by φ. Each ring is one gap from the next and narrower on both sides by the band width divided by φ⁵ per ring. Beyond the second ring an item shrinks to nothing at the area's edge. Each picture fills its ring, cropped around its centre.

**Large view.** A photo shows at 480x480. A video is as large as the area under the bar allows for the way the device is held, upright for the viewer, and rotates with the device:

| | Upright or upside down (480x543) | Button edge up or down (640x383) |
|---|---|---|
| Recorded upright, 720x1280 | 305x543 | 215x383 |
| Recorded sideways, 1280x720 | 480x270 | 640x360 |

In the large view the wheel moves the item the way the Stage theme moves an icon (`ui::picture::carousel`): the item follows the wheel by its leading edge for up to 81 px until the next picture is loaded, then flies off while the next swings in, never overlapping it. The touch on the robot moves a photo the same way to its other version. In the large view an item shows its position for a second at the bottom (`2/7`; for a video `2/7 0:05`). `NO PICTURES`, `NO VIDEOS` and `UNREADABLE` stay.

| Control | Grid | Photo, large | Video, large |
|---|---|---|---|
| Wheel | down: next older item, up: newer; on the drum up: next older, from below | up: next older, from below; down: newer, from above | the same with videos, also while one plays |
| Button | shows the selected item large | back to the grid | play, pause, resume; after the end, from the start |
| Touch on the robot | switches the photos between original and art; nothing for videos | the same, for that photo | nothing |
| Touch on a stamped icon | arms it; a second touch acts, `send` the first; on the drum only the selected item at rest has icons | the same | no icons |
| Swipe from the left | back to the stage or menu | back to the grid, the item still selected | the same |

**Stamped icons.** Each item has `delete` at its bottom right and `send` in the middle of its foot, and a photo `frame_reload` at its bottom left, one gap in from the edges. The icons appear on every thumbnail of a grid, on the drum's selected item at rest, and on a photo shown large. Each is the icon file drawn in a 37 px box (31 px live area) in the interface colour, with a 3 px dark outline around its lit pixels (`r1_funcore::gfx::stamp`). A touch arms the icon (red); a second touch on it acts; any other input disarms it; `send` acts at its first touch, which loses nothing and costs nothing. An icon acts on the item whose picture it stood on at the touch: the gallery hands every small picture and every large one with its item's file, the interface names that file to the gallery (`gallery::Errand`), and the gallery finds the item by it in the roll as it then stands (`Roll::errands`), so that a roll that has grown or shrunk meanwhile changes nothing of which item it is; one whose file is gone by then is passed over. Delete removes the photo and its art, or the video, and the grid is rebuilt; the pixel cam lets go of a deleted photo and deletes its copy and any art saved after the roll was read (`magic::Order::Forget`). The files that go, their order and the pixel cam's order are `Item::delete_steps` (`r1_funcore::domain::gallery::items`). `frame_reload` sends the photo to the pixel cam again, its copy made anew by the darkroom ([Camera](../firmware/camera.md#photo)), at the cost of a first redraw; the new art replaces the old. `send` sends the photo as the roll shows it at the touch, its pixel art, or the photo as taken where the originals show or it has no art (`Item::target`, while `delete` and `frame_reload` act on the photo as taken), or the video: the gallery reads that photo's size in pixels or that video's size in bytes (`Stamp::Send`, `gallery::Sending`) and the interface opens Signal's conversations with `SEND TO` in the bar, the item waiting for the conversation it goes to ([Interface](interface.md#signal)); a video larger than Signal's apps send, about 50 s, shows `TOO LARGE TO SEND` for a second in its place ([Signal](signal.md#attachments)). On failure `NOT DELETED`, `NO PIXELS` or, for `send`, `UNREADABLE` shows for a second.

**Bar.** It shows the roll's name and the selected item's position and status (`3 of 14`, `2 of 5 0:05`, `1 of 22 WAITING`, `UNREADABLE`). In the Companion theme it shows the roll's contents (`24 pictures.`, `One video.`, `No pictures yet.`). In the Terminal theme it shows the path; the pictures have no lines around them, the selected one sits under an inverted band with its position (`1/24`), and the status line shows the position (`1 of 24`) and `MAGIC ON` or `MAGIC OFF`. A roll opens on its newest item. While a video plays, the device does not sleep on its own.
