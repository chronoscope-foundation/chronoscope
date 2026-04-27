# About the project

## Why was Chronoscope built?

Buildings are some of the largest things humans build, and we take them for granted because they're everywhere. Hundreds of people, heavy machinery, structures hundreds of feet tall, expected to last decades or longer, with enormous variation in engineering, purpose, and architecture. They outlast the people who built them, and that persistence makes the built environment one of the most powerful ways to contextualize history. The interest in inhabiting the past through physical places is deeply common, from then-and-now photographs to urban exploration to millions traveling to walk through old cities. What's been missing is a way to explore it all at scale. Recent AI advances make it possible to build the knowledge base from existing open datasets without needing a community to manually annotate millions of images first. For the full story, see the [About page](/about).

## How does Chronoscope handle dates and locations that aren't precise?

Many historical sources give approximate information: "circa 1920s," "somewhere near the waterfront," "between 1914 and 1918." Instead of forcing falsely exact dates or coordinates, Chronoscope keeps the imprecision and works with it. As more evidence arrives, those ranges narrow automatically. A photo captioned "1920s" combined with a city record from 1923 can pin a building's existence to a specific year without either source being that precise on its own.

## How does the AI work?

AI models analyze images in a pipeline of focused steps. First, individual buildings are outlined in each photo so they can be studied separately. Then a visual similarity engine finds the same building across photos from different eras. Finally, a vision-language model describes what it sees: building type, approximate age, architectural style, visible damage. Each step is independently checkable, and every AI-generated conclusion is grounded in the specific photo or source it came from. For more technical details on the specific models used, see our [GitHub documentation](https://github.com/copumpkin/chronoscope).

## Can the AI make mistakes?

Yes, and the system is designed around that assumption. Every AI-generated claim requires a citation to the source material it was derived from. Citations are verified in two ways: first, a simple check that the cited content actually exists at the linked source. Second, a separate AI model evaluates whether the evidence actually supports the claim. Human edits go through the same scrutiny. This makes it hard for errors, misinformation, or deliberate disinformation to spread unchecked. Hard doesn't mean impossible, which is why Chronoscope works like a wiki: if you see something wrong, flag it or fix it. The system is designed to make that easy and even fun.

## How is Chronoscope different from OpenStreetMap, OpenHistoricalMap, Wikidata, Pleiades, and similar projects?

Most existing projects are organized around a map, a gazetteer, or a structured fact base. Chronoscope is organized around **media and the things visible in it**: photographs, maps, drawings, and the buildings and places they depict over time. We use data and identifiers from projects like OpenStreetMap, Wikidata, and OpenHistoricalMap, and we intend to contribute data back as we build confidence in our results. See [Related work and where we fit](/related-work) for details on each project and how we integrate.

# Safety and trust

## How does Chronoscope handle privacy and safety?

Chronoscope is about buildings and places, not people. But detailed information about abandoned or vulnerable sites could attract vandalism or theft, so we design around these risks from the start.

Sensitive locations (abandoned buildings, structures in conflict zones, active urban exploration sites) support distance-based visibility: precise coordinates are only revealed when you're physically nearby or have earned sufficient trust through contributions. Sites can also become fully public after they're no longer sensitive. Every change is versioned with full history, and tools for detecting and reverting vandalism are built in.

Contributors operate under a trust system. New users' submissions are moderated. As your track record builds, you earn faster publishing and eventually moderation privileges. The goal is to make history accessible without creating a playbook for bad actors.

# Contributing

## Are you interested in collaborations with historians and researchers?

Absolutely. Chronoscope is built to complement existing research, not replace it. If you're a historian, archivist, or researcher with domain expertise, we'd love to hear from you. We're especially interested in ingesting complete collections from historical photo archives and institutional datasets. We're building tools to make it easy to bring in large datasets without manual annotation. If you have a collection you think belongs here, [reach out on GitHub](https://github.com/copumpkin/chronoscope/issues).

## Is it open source?

Yes! Chronoscope is open source under the MIT license. The codebase includes the API server, analysis pipeline, iOS app, and this web frontend, all sharing types and contracts via OpenAPI.

## How can I contribute?

We're working on the first release of the full site, which will let you contribute directly to the knowledge graph: linking photos to locations, adding historical context, and resolving conflicts in the data. In the meantime, code contributions are welcome. Check out the [GitHub repository](https://github.com/copumpkin/chronoscope) for the codebase, data ingestion pipelines, and discussion in Issues.
