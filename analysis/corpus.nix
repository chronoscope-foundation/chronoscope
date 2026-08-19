# Corpus manifest — curated test images for the analysis pipeline.
#
# Pure data (no imports). Nix evaluates this to JSON for Rust consumption
# via builtins.toJSON. Edit this file to add/modify corpus entries.
#
# After adding new URLs, run: just corpus-hash
{
  clusters = {
    triple-palace = {
      members = [
        "5th-ave-photochrom[0]"
        "5th-ave-photochrom[1]"
        "vanderbilt-triple-palace[0]"
        "vanderbilt-triple-palace[1]"
        "vanderbilt-triple-palace[2]"
      ];
    };
  };
  images = {
    "5th-ave-1885-levy" = {
      description = "5th Ave at 54th St, NYC, 1885 — Albert Levy. Low-res but rare view showing opposite side of St. Thomas' Church.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/f/f3/5th_avenue_-_54th_NY_1885_Albert_Levy.jpg";
    };
    "5th-ave-1890-from-st-patricks" = {
      description = "5th Ave and Vanderbilt Mansions seen from St. Patrick's Cathedral, NYC, 1890.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/0/07/Fifth_Avenue_and_the_Vanderbilt_Mansions_seen_from_St.Patrick%27s_Cathedral%2C_New_York_1890.jpg";
    };
    "5th-ave-1895-from-52nd" = {
      description = "5th Ave from 52nd St, NYC, c. 1895 — Zeno Fotografie.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/8/8f/Amerikanischer_Photograph_um_1895_-_Fifth_Avenue_von_der_52nd_Street_%28Zeno_Fotografie%29.jpg";
    };
    "5th-ave-1900-vanderbilt" = {
      description = "5th Ave and Vanderbilt Mansions, NYC, 1900.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/1/13/Fifth_Avenue_and_Vanderbilt_Mansions%2C_New_York_1900.jpg";
    };
    "5th-ave-1908" = {
      description = "5th Ave, NYC, 1908.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/9/91/Fifth_Avenue_1908.jpg";
    };
    "5th-ave-easter-1898" = {
      description = "5th Ave Easter Parade, NYC, 1898.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/f/ff/Fifth_Avenue_Easter_Parade%2C_1898.jpg";
    };
    "5th-ave-photochrom" = {
      description = "5th Ave, NYC - photochrom, Detroit Publishing Co.";
      regions = 8;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/det/4a30000/4a31000/4a31800/4a31817v.jpg";
    };
    "abandoned-chapel.0" = {
      description = "Abandoned chapel";
      reddit_index = 0;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1otcpmj/abandoned_chapel/";
    };
    "abandoned-chapel.1" = {
      description = "Abandoned chapel";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1otcpmj/abandoned_chapel/";
    };
    "abandoned-chapel.2" = {
      description = "Abandoned chapel";
      reddit_index = 2;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1otcpmj/abandoned_chapel/";
    };
    "abandoned-factory.0" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 0;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-factory.1" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-factory.2" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 2;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-factory.3" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 3;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-factory.4" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 4;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-factory.5" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 5;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-factory.6" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 6;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-factory.7" = {
      description = "Abandoned fiberglass factory - Superior Fibers LLC";
      reddit_index = 7;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1osyclq/abandoned_fiberglass_factory_superior_fibers_llc/";
    };
    "abandoned-house.0" = {
      description = "Abandoned house - 'Who do you think lived here?'";
      reddit_index = 0;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1onms6w/who_do_you_think_lived_here/";
    };
    "abandoned-house.1" = {
      description = "Abandoned house - 'Who do you think lived here?'";
      reddit_index = 1;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1onms6w/who_do_you_think_lived_here/";
    };
    "abandoned-house.2" = {
      description = "Abandoned house - 'Who do you think lived here?'";
      reddit_index = 2;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1onms6w/who_do_you_think_lived_here/";
    };
    "abandoned-house.3" = {
      description = "Abandoned house - 'Who do you think lived here?'";
      reddit_index = 3;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1onms6w/who_do_you_think_lived_here/";
    };
    "abandoned-house.4" = {
      description = "Abandoned house - 'Who do you think lived here?'";
      reddit_index = 4;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1onms6w/who_do_you_think_lived_here/";
    };
    "abandoned-house.5" = {
      description = "Abandoned house - 'Who do you think lived here?'";
      reddit_index = 5;
      type = "single";
      url = "https://www.reddit.com/r/abandoned/comments/1onms6w/who_do_you_think_lived_here/";
    };
    "abandoned-radio-telescopes.0" = {
      description = "Abandoned radio telescopes - scientific/industrial infrastructure";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1r2zag7/abandoned_radio_telescopes_oc/";
    };
    "abandoned-radio-telescopes.1" = {
      description = "Abandoned radio telescopes - scientific/industrial infrastructure";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1r2zag7/abandoned_radio_telescopes_oc/";
    };
    "abandoned-radio-telescopes.2" = {
      description = "Abandoned radio telescopes - scientific/industrial infrastructure";
      reddit_index = 2;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1r2zag7/abandoned_radio_telescopes_oc/";
    };
    "abandoned-radio-telescopes.3" = {
      description = "Abandoned radio telescopes - scientific/industrial infrastructure";
      reddit_index = 3;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1r2zag7/abandoned_radio_telescopes_oc/";
    };
    "abandoned-radio-telescopes.4" = {
      description = "Abandoned radio telescopes - scientific/industrial infrastructure";
      reddit_index = 4;
      type = "single";
      url = "https://www.reddit.com/r/urbanexploration/comments/1r2zag7/abandoned_radio_telescopes_oc/";
    };
    angkor-wat-moat = {
      description = "Angkor Wat viewed from moat, Cambodia";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/b/b9/Angkor_Wat_from_moat.jpg";
    };
    angkor-wat-reflection = {
      description = "Angkor Wat front western approach with reflecting pool, Cambodia";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/2/2b/Angkor_Wat%2C_Camboya%2C_2013-08-15%2C_DD_032.JPG";
    };
    angkor-wat-west = {
      description = "Angkor Wat west side view, Cambodia";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/8/8b/Angkor_Wat_W-Seite.jpg";
    };
    arc-de-triomphe = {
      description = "Arc de Triomphe de l'Etoile, Paris - neoclassical triumphal arch";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/5/5f/Arc_de_Triomphe%28Paris%29.JPG";
    };
    "art-deco-gas-station.0" = {
      count = 2;
      description = "Restored Art Deco gas station - before/after";
      expect_similar = false;
      layout = "vertical";
      reddit_index = 0;
      subimages = {
        "1" = {
          regions = 1;
        };
      };
      type = "composite";
      url = "https://www.reddit.com/r/ArtDeco/comments/1oulkkp/they_restored_an_old_gas_station_that_was_going/";
    };
    asphalt-modern-street = {
      description = "Modern street with asphalt road - Highsmith";
      regions = 4;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/10000/10054v.jpg";
    };
    "bank-of-okeechobee.0" = {
      description = "Bank of Okeechobee - then and now";
      reddit_index = 0;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1ouavg6/bank_of_okeechobee/";
    };
    "bank-of-okeechobee.1" = {
      description = "Bank of Okeechobee - then and now";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1ouavg6/bank_of_okeechobee/";
    };
    bethlehem-steel-haer = {
      description = "Bethlehem Steel blast furnace plant - HAER PA-386-D";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/habshaer/pa/pa3300/pa3390/photos/359754pv.jpg";
    };
    bicycle-amsterdam = {
      description = "Bicycles and canal houses, Amsterdam";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/2/24/BikesInAmsterdam_2004_SeanMcClean.jpg/1280px-BikesInAmsterdam_2004_SeanMcClean.jpg";
    };
    blue-mosque-istanbul = {
      description = "Sultan Ahmed Mosque (Blue Mosque) exterior, Istanbul, Turkey";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/5/56/Sultan_Ahmed_Mosque_Istanbul_Turkey_retouched.jpg";
    };
    borgund-stave-church = {
      description = "Borgund Stave Church, Norway - medieval wooden church c. 1180 CE";
      regions = 2;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/1/1b/Borgundstavechurch.JPG";
    };
    boro-park-brownstone = {
      description = "Boro Park brownstone row, Brooklyn - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/52800/52895v.jpg";
    };
    boston-lithograph = {
      description = "Bird's eye view of Boston - J. Bachmann lithograph, c. 1850";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/pga/00100/00100v.jpg";
    };
    cambridge-gambrel = {
      description = "Cambridge MD - HABS gambrel roof house";
      regions = 2;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/habshaer/md/md0300/md0383/photos/081567pv.jpg";
    };
    cape-hatteras-lighthouse = {
      description = "Cape Hatteras Lighthouse, Buxton NC - tallest brick lighthouse in the US";
      regions = 2;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/habshaer/nc/nc0400/nc0432/color/361645cv.jpg";
    };
    carnegie-mansion = {
      description = "Carnegie Mansion (now Cooper Hewitt), NYC";
      regions = 3;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/2/28/Carnegie_Mansion_now_Cooper-Hewitt_Museum.jpg/1280px-Carnegie_Mansion_now_Cooper-Hewitt_Museum.jpg";
    };
    carrie-furnace = {
      description = "Carrie Furnace, Rankin PA";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/a/a5/Blast_furnace%2C_Carrie_Furnaces%2C_Rankin_PA_%288907652105%29.jpg";
    };
    chand-baori-stepwell = {
      description = "Chand Baori stepwell, Abhaneri, Rajasthan - 3,500 steps across 13 stories";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/2/24/Chand_Baori_%28Step-well%29_at_Abhaneri.JPG";
    };
    chestnut-st-philly = {
      description = "Chestnut St from 9th St, Philadelphia - Detroit Publishing, c. 1900";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/det/4a00000/4a08000/4a08400/4a08462v.jpg";
    };
    "chicago-federal-building.0" = {
      count = 2;
      description = "Chicago Federal Building - then and now";
      expect_similar = false;
      layout = "vertical";
      reddit_index = 0;
      type = "composite";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1o6cpip/chicago_federal_building/";
    };
    chichen-itza-el-castillo = {
      description = "El Castillo (Temple of Kukulcan), Chichen Itza - seen from east";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/7/7a/Chichen-Itza-Castillo-Seen-From-East.JPG";
    };
    coal-breaker-stereograph = {
      count = 2;
      description = "Coal breaker, Scranton PA - Keystone View stereograph, c. 1905";
      expect_similar = true;
      layout = "side_by_side";
      type = "composite";
      url = "https://tile.loc.gov/storage-services/service/pnp/stereo/1s10000/1s15000/1s15400/1s15410v.jpg";
    };
    "colosseum-early-photo.0" = {
      description = "Early photo of the Colosseum in Rome";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/ancientrome/comments/1ov4vbe/early_photo_of_the_colosseum_in_rome_taken_during/";
    };
    "colosseum-forum.0" = {
      description = "The Colosseum and Roman Forum area in Rome - then and now";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1oqd7jy/the_colosseum_and_roman_forum_area_in_rome/";
    };
    concrete-road-scene = {
      description = "Old Route 66, Mohave County, Arizona - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/54800/54845v.jpg";
    };
    cornell-mcgraw-hall = {
      description = "McGraw Hall, Cornell University - mansard roof, Victorian Gothic";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/b/b0/Cornell_McGraw_Hall_1.jpg/1280px-Cornell_McGraw_Hall_1.jpg";
    };
    cornish-windsor-covered-bridge = {
      description = "Cornish-Windsor Covered Bridge spanning Connecticut River - longest covered bridge in the US";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/e/ec/Cornish-Windsor_Covered_Bridge.jpg";
    };
    dc-mansard-row = {
      description = "DC row houses with mansard roofs - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/10200/10231v.jpg";
    };
    dirt-road-rural = {
      description = "Rural buildings along dirt road, Louisiana, 1927";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/cph/3c20000/3c29000/3c29300/3c29312v.jpg";
    };
    djenne-mosque = {
      description = "Great Mosque of Djenne, Mali - largest adobe building in the world";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/f/f1/MaliDjenn%C3%A9Mosqu%C3%A9e.JPG";
    };
    fence-boundary = {
      description = "Jackson Square fence with Pontalba Buildings, New Orleans - iron fence + brick row buildings";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/7/73/New_Orleans_-_Jackson_Square_%22Pontalba_Buildings_Under_Oak_Tree%22.jpg";
    };
    ferndale-victorian = {
      description = "Ferndale CA Victorian with bay windows - Highsmith";
      regions = 4;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/22200/22237v.jpg";
    };
    fire-hydrant-scene = {
      description = "Fire hydrant public art, Fire Museum of Texas, Beaumont - Highsmith";
      regions = 4;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/17100/17179v.jpg";
    };
    fountains-abbey = {
      description = "Fountains Abbey, Yorkshire";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/2/28/Fountains_Abbey_view02_2005-08-27.jpg/1280px-Fountains_Abbey_view02_2005-08-27.jpg";
    };
    frick-house = {
      description = "Frick House (now Frick Collection), NYC";
      regions = 5;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/e/e1/Henry_C_Frick_House_009.JPG/1280px-Henry_C_Frick_House_009.JPG";
    };
    grain-elevator-kiowa = {
      description = "Grain elevator, Kiowa KS - Jack Delano, March 1943, along ATSF Railroad";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/fsa/8d15000/8d15200/8d15285v.jpg";
    };
    guggenheim-construction = {
      description = "Guggenheim Museum under construction, NYC - Gottscho-Schleisner, Nov 1957";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/gsc/5a25000/5a25400/5a25493r.jpg";
    };
    haughwout-building = {
      description = "E.V. Haughwout Building, 488-492 Broadway, SoHo NYC - cast iron facade (1856)";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/b/b5/E.V._Haughwout_Building.JPG/1280px-E.V._Haughwout_Building.JPG";
    };
    holyoke-factories = {
      description = "Norman Paper Mill Tower, Holyoke MA - abandoned factory";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/5/53/Norman_Paper_Mill_Tower%2C_Holyoke%2C_Mass.jpg/1280px-Norman_Paper_Mill_Tower%2C_Holyoke%2C_Mass.jpg";
    };
    huber-breaker-haer = {
      description = "Huber Coal Breaker, Ashley PA - HAER PA-204";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/habshaer/pa/pa2200/pa2251/photos/041285pv.jpg";
    };
    itsukushima-torii = {
      description = "Itsukushima floating torii gate, Miyajima, Hiroshima Prefecture";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/0/0e/Itsukushima_Gate.jpg";
    };
    "jaragua-hotel.0" = {
      description = "Jaragua Hotel by Guillermo Gonzalez, 1942–1985";
      reddit_index = 0;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1one8gi/jaragua_hotel_by_guillermo_gonzalez_19421985/";
    };
    "jaragua-hotel.1" = {
      description = "Jaragua Hotel by Guillermo Gonzalez, 1942–1985";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1one8gi/jaragua_hotel_by_guillermo_gonzalez_19421985/";
    };
    "jaragua-hotel.2" = {
      description = "Jaragua Hotel by Guillermo Gonzalez, 1942–1985";
      reddit_index = 2;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1one8gi/jaragua_hotel_by_guillermo_gonzalez_19421985/";
    };
    "jaragua-hotel.3" = {
      description = "Jaragua Hotel by Guillermo Gonzalez, 1942–1985";
      reddit_index = 3;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1one8gi/jaragua_hotel_by_guillermo_gonzalez_19421985/";
    };
    "jaragua-hotel.4" = {
      description = "Jaragua Hotel by Guillermo Gonzalez, 1942–1985";
      reddit_index = 4;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1one8gi/jaragua_hotel_by_guillermo_gonzalez_19421985/";
    };
    keystone-arch = {
      description = "Admiralty Gate, Birgu, Malta - keystone arch detail";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/9/9d/Admiralty_Gate_%28Birgu%2C_Malta%29_02.jpg/1280px-Admiralty_Gate_%28Birgu%2C_Malta%29_02.jpg";
    };
    kings-colorgraph-nyc = {
      description = "King's Color-graphs of New York City, plate 20 — composite page with multiple subimages including current St. Thomas' Church. Subimage layout is irregular/complex.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/c/cb/King%27s_Color-graphs_of_New_York_City20.jpg";
    };
    kiyomizudera-kyoto = {
      description = "Kiyomizu-dera Buddhist temple, Kyoto - wooden stage and three-storied pagoda";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/9/93/Kyoto-Kiyomizu_Temple-2.JPG";
    };
    lalibela-st-george = {
      description = "Church of Saint George (Bete Giyorgis), Lalibela, Ethiopia - rock-hewn monolith";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/a/a4/Bete_Giyorgis_01.jpg";
    };
    loc-architectural-drawing = {
      description = "US Capitol architectural drawing - William Thornton, 1793";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/cph/3b50000/3b51000/3b51700/3b51718r.jpg";
    };
    loc-biltmore-1894 = {
      description = "Biltmore Estate, 1894 - photographed by Richard Morris Hunt";
      regions = 1;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/ds/09700/09773v.jpg";
    };
    machu-picchu = {
      description = "Machu Picchu overview with Huayna Picchu backdrop, Peru";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/c/ca/Machu_Picchu%2C_Peru_%282018%29.jpg";
    };
    manhattan-panorama-1854 = {
      description = "City of New York and Environs - panoramic lithograph, 1854";
      type = "single";
      url = "https://iiif.nypl.org/iiif/3/psnypl_prn_1006/full/%5E!2560,2560/0/default.jpg";
    };
    marble-courthouse = {
      description = "US Supreme Court Building, DC - Highsmith";
      regions = 1;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/11800/11889v.jpg";
    };
    market-st-sf-photochrom = {
      description = "Market St, San Francisco - photochrom";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/ppmsca/39500/39502v.jpg";
    };
    meenakshi-gopuram = {
      description = "Meenakshi Amman Temple gopuram at dusk, Madurai, Tamil Nadu";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/0/03/Meenakshi_Temple_Gopuram_at_dusk.jpg";
    };
    met-cloisters = {
      description = "The Met Cloisters, NYC";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/a/ac/The_Met_Cloisters.jpg/1280px-The_Met_Cloisters.jpg";
    };
    mont-saint-michel-aerial = {
      description = "Mont-Saint-Michel - aerial view";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/8/8a/Mont_st_michel_aerial.jpg/1280px-Mont_st_michel_aerial.jpg";
    };
    mont-saint-michel-cloister = {
      description = "Mont-Saint-Michel cloister";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/d/dd/Le_Mont_Saint-Michael_Cloister.JPG/1280px-Le_Mont_Saint-Michael_Cloister.JPG";
    };
    national-building-museum = {
      description = "National Building Museum atrium, DC - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/12300/12373v.jpg";
    };
    "newcom-tavern.0" = {
      description = "Newcom Tavern, Dayton, OH - built 1796, still standing";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1opnmkk/newcom_tavern_dayton_oh_built_1796_still_standing/";
    };
    "newcom-tavern.1" = {
      description = "Newcom Tavern, Dayton, OH - built 1796, still standing";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1opnmkk/newcom_tavern_dayton_oh_built_1796_still_standing/";
    };
    "newcom-tavern.2" = {
      description = "Newcom Tavern, Dayton, OH - built 1796, still standing";
      reddit_index = 2;
      regions = 3;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1opnmkk/newcom_tavern_dayton_oh_built_1796_still_standing/";
    };
    "newcom-tavern.3" = {
      description = "Newcom Tavern, Dayton, OH - built 1796, still standing";
      reddit_index = 3;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1opnmkk/newcom_tavern_dayton_oh_built_1796_still_standing/";
    };
    nola-courtyard = {
      description = "Pirate's Alley, French Quarter, New Orleans - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/16300/16364v.jpg";
    };
    nyc-1909-balloon = {
      description = "New York City in 1909 - photographed from a balloon, aerial panorama";
      type = "single";
      url = "https://iiif.nypl.org/iiif/3/5059954/full/max/0/default.jpg";
    };
    nyc-panorama-lithograph = {
      description = "Panorama of New York and Vicinity - bird's eye lithograph";
      type = "single";
      url = "https://iiif.nypl.org/iiif/3/1659270/full/6417,/0/default.jpg";
    };
    nyc-winter-street = {
      description = "Winter street scene near church, NYC - sepia photograph, pedestrians in snow";
      type = "single";
      url = "https://iiif.nypl.org/iiif/3/801650/full/%5E!2560,2560/0/default.jpg";
    };
    "old-train-station.0" = {
      description = "Old train station - 20th century vs 21st century";
      reddit_index = 0;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oul4to/old_train_station_20th_century21st_century_santa/";
    };
    "old-train-station.1" = {
      description = "Old train station - 20th century vs 21st century";
      reddit_index = 1;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oul4to/old_train_station_20th_century21st_century_santa/";
    };
    "old-train-station.2" = {
      description = "Old train station - 20th century vs 21st century";
      reddit_index = 2;
      regions = 3;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oul4to/old_train_station_20th_century21st_century_santa/";
    };
    "paris-pavilions-1900.0" = {
      description = "Turkish and American pavilions at the 1900 Paris Exposition";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/HistoryPorn/comments/1ogpb7d/turkish_and_american_pavilions_at_the_1900_paris/";
    };
    "parsons-flatiron-holyoke.0" = {
      count = 2;
      description = "Parsons Block - the Flatiron Building, Holyoke - then and now vertical composite";
      expect_similar = false;
      layout = "vertical";
      reddit_index = 0;
      type = "composite";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1ouy1fj/parsons_block_the_flatiron_building_holyoke/";
    };
    "parsons-flatiron-holyoke.1" = {
      description = "Parsons Block - the Flatiron Building, Holyoke - then and now";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/OldPhotosInRealLife/comments/1ouy1fj/parsons_block_the_flatiron_building_holyoke/";
    };
    pentagon-construction = {
      description = "Pentagon under construction, July 1942";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/5/5e/Pentagon_construction.jpg";
    };
    pilaster-detail = {
      description = "Building facade with pilasters, DC - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/15400/15416v.jpg";
    };
    plaza-5th-ave-postcard = {
      description = "Plaza, 5th Ave and 59th St, New York - hand-colored postcard, 760x500, printed in Germany";
      type = "single";
      url = "https://iiif.nypl.org/iiif/3/836593/full/max/0/default.jpg";
    };
    pont-du-gard = {
      description = "Pont du Gard Roman aqueduct, Nimes, France - c. 40-60 AD";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/8/8a/Pont_du_gard_v1_082005.JPG";
    };
    portico-columns = {
      description = "National Archives portico, DC - Highsmith";
      regions = 1;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/12500/12561v.jpg";
    };
    "porticus-octaviae.0" = {
      count = 2;
      description = "Porticus Octaviae, Rome - then and now composite";
      expect_similar = false;
      layout = "side_by_side";
      reddit_index = 0;
      type = "composite";
      url = "https://www.reddit.com/r/rome/comments/1ontxzb/porticus_octaviae/";
    };
    potala-palace-front = {
      description = "Potala Palace frontal view with stairway approach, Lhasa, Tibet";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/d/dd/Palacio_de_Potala_-_02.JPG";
    };
    potala-palace-panoramic = {
      description = "Potala Palace panoramic wide view in landscape context, Lhasa, Tibet";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/1/18/Tibet_-_Lhasa_-_Potala_Palace_-_6406925781.jpg";
    };
    potala-palace-sw = {
      description = "Potala Palace from Chagpo Ri (southwest view), Lhasa, Tibet";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/4/4f/Potala.jpg";
    };
    "pulteney-bridge.0" = {
      description = "1774 Palladian-style Pulteney Bridge, Bath";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/ArchitecturePorn/comments/1osgwrx/1774_palladianstyle_pulteney_bridge_reflected_in/";
    };
    quoins-lintel = {
      description = "Georgian townhouse detail - quoins and lintels";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/12300/12370v.jpg";
    };
    rievaulx-abbey = {
      description = "Rievaulx Abbey ruins, Yorkshire";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/0/06/RievaulxAbbey-wyrdlight-24588.jpg/1280px-RievaulxAbbey-wyrdlight-24588.jpg";
    };
    round-window-church = {
      description = "Rose window, Cathedral of Lodi, Italy";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/1/1b/Rose-window-Cathedral-Lodi.JPG/1280px-Rose-window-Cathedral-Lodi.JPG";
    };
    "san-francisco-convent.0" = {
      description = "Casa Grande de San Francisco Convent, 1411–1843";
      reddit_index = 0;
      regions = 2;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1nv9r2g/casagrande_de_san_francisco_convent_14111843/";
    };
    "san-francisco-convent.1" = {
      description = "Casa Grande de San Francisco Convent, 1411–1843";
      reddit_index = 1;
      regions = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1nv9r2g/casagrande_de_san_francisco_convent_14111843/";
    };
    "san-francisco-convent.2" = {
      description = "Casa Grande de San Francisco Convent, 1411–1843";
      reddit_index = 2;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1nv9r2g/casagrande_de_san_francisco_convent_14111843/";
    };
    "san-francisco-convent.3" = {
      description = "Casa Grande de San Francisco Convent, 1411–1843";
      reddit_index = 3;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1nv9r2g/casagrande_de_san_francisco_convent_14111843/";
    };
    "san-remo.0" = {
      description = "The San Remo, Central Park West, NYC";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/ArchitecturePorn/comments/1oxoxrc/the_san_remo_new_york_city/";
    };
    "san-sebastian-gate.0" = {
      description = "San Sebastian Gate in the Aurelian Walls of Rome";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/ancientrome/comments/1ohh16r/the_san_sebasti%C3%A1n_gate_in_the_aurelian_walls_of/";
    };
    sanborn-manhattan-1907-33 = {
      description = "Sanborn fire insurance map, Manhattan Vol. 6 (1907), plate 33 — 5th to 6th Ave, W 52nd-55th St. Covers Vanderbilt triple-palace block.";
      type = "single";
      url = "https://tile.loc.gov/image-services/iiif/service:gmd:gmd380m:g3804m:g3804nm:g3804nm_g06116190706:06116_06_1907-0033/full/pct:25/0/default.jpg";
    };
    sanborn-manhattan-1910-46 = {
      description = "Sanborn fire insurance map, Manhattan Vol. 4 (1910), plate 46 — 5th to 6th Ave, W 49th-52nd St.";
      type = "single";
      url = "https://tile.loc.gov/image-services/iiif/service:gmd:gmd380m:g3804m:g3804nm:g3804nm_g06116191004:06116_04_1910-0046/full/pct:25/0/default.jpg";
    };
    sanborn-manhattan-1910-48 = {
      description = "Sanborn fire insurance map, Manhattan Vol. 4 (1910), plate 48 — east side of 5th Ave to Park Ave, 49th-52nd St. Includes St. Patrick's Cathedral footprint.";
      type = "single";
      url = "https://tile.loc.gov/image-services/iiif/service:gmd:gmd380m:g3804m:g3804nm:g3804nm_g06116191004:06116_04_1910-0048/full/pct:25/0/default.jpg";
    };
    scaffolding-renovation = {
      description = "Orion Building with scaffolding during renovation";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/4/41/Orion_Building_scaffolding.JPG/1280px-Orion_Building_scaffolding.JPG";
    };
    "schwerin-palace.0" = {
      description = "Schwerin Palace, Germany - UNESCO World Heritage";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/ArchitecturePorn/comments/1op9z1i/schwerin_palace_germany_a_unesco_world_heritage/";
    };
    seagram-building = {
      description = "Seagram Building, 375 Park Ave, NYC - Mies van der Rohe";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/8/81/Seagrambuilding.JPG/1280px-Seagrambuilding.JPG";
    };
    seattle-street-historic = {
      description = "Historic Seattle street scene - dating challenge, urban streetscape with period vehicles and signage";
      type = "single";
      url = "https://i.redd.it/hhow9rgakrjg1.jpeg";
    };
    shekar-dzong-1921 = {
      description = "Shekar Dzong, Monastery and village in 1921 - Everest Reconnaissance Expedition";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/a/a6/Shekar_Dzong%2C_Shekar_Monastery_and_Shekar_%28village%29_in_1921.jpg";
    };
    shekar-dzong-burgberg = {
      description = "Shekar Dzong fortress hill close-up showing vertical extent of ruins, Tibet";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/3/32/Shelkar-Dzong-04-Burgberg-2014-gje.jpg";
    };
    shekar-dzong-panoramic = {
      description = "Shekar Dzong (Shelkar/Xegar), Tingri County, Tibet - panoramic view of fortress ruins";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/f/f0/Shelkar-Dzong-06-2014-gje.jpg";
    };
    sloss-furnaces = {
      description = "Sloss Furnaces, Birmingham AL";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/1/1c/Blast_Furnace%2C_Sloss_Furnaces%2C_Birmingham_AL%2C_West_view_20160714_1.jpg";
    };
    soho-fire-escapes = {
      description = "SoHo apartments with fire escapes, NYC - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/12400/12461v.jpg";
    };
    soho-water-tower = {
      description = "Rooftop water towers in SoHo, Manhattan";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/2/2f/Rooftop_water_towers_in_SoHo%2C_Manhattan.jpg/1280px-Rooftop_water_towers_in_SoHo%2C_Manhattan.jpg";
    };
    st-basils-vertical = {
      description = "St. Basil's Cathedral, Moscow - tall vertical view emphasizing dome layering";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/9/9e/Moscow_05-2012_StBasilCathedral.jpg";
    };
    st-basils-wide = {
      description = "St. Basil's Cathedral, Red Square, Moscow - wide horizontal view";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/c/c7/St.Basil-Moscow_Red_Square.JPG";
    };
    st-thomas-church-5th-ave = {
      description = "St. Thomas' Church, 5th Ave looking south from 54th St, NYC — glass plate negative c. 1876. Empty lot next to church is later built on, providing a temporal constraint against other corpus photos of this block. NYHS via DCMNY.";
      type = "single";
      url = "https://dcmny.org/cantaloupe/iiif/2/924%2Fimage-obj-e87287a0-c55a-45f1-87a8-51b67f0cbab6.jp2/full/full/0/default.jpg";
    };
    st-thomas-church-modern = {
      description = "St. Thomas' Church (current building, 1914), 5th Ave and 53rd St, NYC — modern photo.";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/c/ca/New_York_-_Manhattan_-_Saint_Thomas_Church.jpg";
    };
    st-thomas-church-stereo = {
      count = 2;
      description = "St. Thomas' Church, 5th Ave, NYC — stereograph showing the pre-1905 church. NYPL.";
      expect_similar = true;
      layout = "side_by_side";
      type = "composite";
      url = "https://iiif.nypl.org/iiif/3/G91F200_058F/full/max/0/default.jpg";
    };
    stucco-building = {
      description = "US Custom House, San Ysidro CA - Spanish Revival stucco, 1933 - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/71900/71981v.jpg";
    };
    taj-mahal-front = {
      description = "Taj Mahal south side with reflecting pool, Agra, India";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/f/f0/Taj_Mahal_Front.JPG";
    };
    taj-mahal-rear = {
      description = "Taj Mahal rear view from Mehtab Bagh across Yamuna River, India";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/8/8d/Mehtab_Bagh_facing_Taj_Mahal.JPG";
    };
    taj-mahal-sunrise = {
      description = "Taj Mahal west side at sunrise, Agra, India";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/b/b2/Taj_Mahal_Tomb_at_sunrise.JPG";
    };
    terra-cotta-detail = {
      description = "Terra cotta detail, Albert Lea State Bank Building, Minnesota - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/59800/59804v.jpg";
    };
    tintern-abbey = {
      description = "Tintern Abbey, Wales";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/b/b0/Tintern_Abbey_and_Courtyard.jpg/1280px-Tintern_Abbey_and_Courtyard.jpg";
    };
    "toplerhaus.0" = {
      description = "Toplerhaus, 1590–1945, Nuremberg, Germany";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oum7eb/toplerhaus_15901945_nuremberg_germany/";
    };
    "toplerhaus.1" = {
      description = "Toplerhaus, 1590–1945, Nuremberg, Germany";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oum7eb/toplerhaus_15901945_nuremberg_germany/";
    };
    "toplerhaus.2" = {
      description = "Toplerhaus, 1590–1945, Nuremberg, Germany";
      reddit_index = 2;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oum7eb/toplerhaus_15901945_nuremberg_germany/";
    };
    "toplerhaus.3" = {
      description = "Toplerhaus, 1590–1945, Nuremberg, Germany";
      reddit_index = 3;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oum7eb/toplerhaus_15901945_nuremberg_germany/";
    };
    traffic-signal-intersection = {
      description = "Historic Third Ward, Milwaukee - lamppost and traffic signal - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/40200/40242v.jpg";
    };
    tunkhannock-viaduct = {
      description = "Tunkhannock Viaduct, PA - 240 ft tall concrete railroad viaduct (1915), DL&W Railroad";
      regions = 5;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/0/0d/Tunkhannock_Viaduct.jpg";
    };
    twin-branch-wv = {
      description = "Twin Branch WV - boarded-up mining town, FSA";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/fsa/8a38000/8a38800/8a38804v.jpg";
    };
    "urbanhell-village.0" = {
      description = "Where is this village/town/city?";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/UrbanHell/comments/1or06lp/where_is_this_villagetowncity/";
    };
    "urbanhell-village.1" = {
      description = "Where is this village/town/city?";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/UrbanHell/comments/1or06lp/where_is_this_villagetowncity/";
    };
    vanderbilt-cornelius-stereograph = {
      count = 2;
      description = "Cornelius Vanderbilt II House - stereograph, Alfred S. Campbell, 1896";
      expect_similar = true;
      layout = "side_by_side";
      subimages = {
        "0" = {
          regions = 4;
        };
      };
      type = "composite";
      url = "https://tile.loc.gov/storage-services/service/pnp/stereo/1s00000/1s07000/1s07500/1s07549v.jpg";
    };
    vanderbilt-cornelius-wiki = {
      description = "Cornelius Vanderbilt II House, 1 W 57th St, NYC";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/thumb/5/53/Cornelius_Vanderbilt_II_House.jpg/1280px-Cornelius_Vanderbilt_II_House.jpg";
    };
    vanderbilt-triple-palace = {
      description = "William H. Vanderbilt Triple Palace, 640 Fifth Ave, NYC";
      regions = 4;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/6/6b/640_%26_642_5th_Avenue_and_2_West_52nd_Street%2C_New_York%2C_NY.jpg";
    };
    vanderbilt-wk-660-5th = {
      description = "William K. Vanderbilt House, 660 Fifth Ave, NYC - Richard Morris Hunt, c. 1885";
      regions = 4;
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/ds/10700/10771v.jpg";
    };
    vanderbilt-wk-stereograph = {
      count = 2;
      description = "William K. Vanderbilt House - stereograph, William H. Rau, c. 1903";
      expect_similar = true;
      layout = "side_by_side";
      subimages = {
        "0" = {
          regions = 5;
        };
        "1" = {
          regions = 5;
        };
      };
      type = "composite";
      url = "https://tile.loc.gov/storage-services/service/pnp/stereo/1s00000/1s07000/1s07500/1s07550v.jpg";
    };
    "wall-st-demolished.0" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 0;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    "wall-st-demolished.1" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 1;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    "wall-st-demolished.2" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 2;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    "wall-st-demolished.3" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 3;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    "wall-st-demolished.4" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 4;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    "wall-st-demolished.5" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 5;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    "wall-st-demolished.6" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 6;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    "wall-st-demolished.7" = {
      description = "Demolished old-world skyscrapers on Wall St.";
      reddit_index = 7;
      type = "single";
      url = "https://www.reddit.com/r/Lost_Architecture/comments/1oq8676/demolished_oldworld_skyscrapers_on_wall_st_in/";
    };
    wat-arun-bangkok = {
      description = "Wat Arun (Temple of Dawn), Bangkok - Khmer-style central prang";
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/9/90/Bangkok_Wat_Arun_P1130114.JPG";
    };
    wild-goose-pagoda-xian = {
      description = "Giant Wild Goose Pagoda, Xi'an - Tang dynasty brick pagoda (652/701 CE)";
      regions = 1;
      type = "single";
      url = "https://upload.wikimedia.org/wikipedia/commons/1/13/Giant_Wild_Goose_Pagoda.jpg";
    };
    willard-hotel-lobby = {
      description = "Willard Hotel lobby, Washington DC - Highsmith";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/highsm/14100/14199v.jpg";
    };
    wilmington-street = {
      description = "Wilmington DE busy street scene - streetcars, autos, horse-drawn carriages, c. 1900";
      type = "single";
      url = "https://tile.loc.gov/storage-services/service/pnp/cph/3a40000/3a45000/3a45800/3a45818r.jpg";
    };
  };
}
