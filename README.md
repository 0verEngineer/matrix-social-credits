<div id="top"></div>


<!-- PROJECT SHIELDS -->
[![Contributors][contributors-shield]][contributors-url]
[![Forks][forks-shield]][forks-url]
[![Stargazers][stars-shield]][stars-url]
[![Issues][issues-shield]][issues-url]
[![GPLv3 License][license-shield]][license-url]


<!-- PROJECT LOGO -->
<br />
<div align="center">

<!-- description -->
Matrix bot for a social credit system
<!-- description end -->

  <p align="center">
    <br />
    <a href="https://codeberg.org/OverEngineer/matrix-social-credits">Codeberg</a>
    ·
    <a href="https://github.com/0verEngineer/matrix-social-credits">Github</a>
    .
    <a href="https://hub.docker.com/r/0verengineer/matrix-social-credits">Docker Hub</a>
    .
    <a href="https://github.com/0verEngineer/matrix-social-credits/issues">Report Bug</a>
    ·
    <a href="https://github.com/0verEngineer/matrix-social-credits/issues">Request Feature</a>
  </p>
</div>


---
<details>
    <summary>Screenshots</summary>
</details>


<!-- TABLE OF CONTENTS -->
<details>
  <summary>Table of Contents</summary>
  <ol>
    <li>
      <a href="#about-the-project">About The Project</a>
    </li>
    <li>
      <a href="#setup">Setup</a>
    </li>
    <li><a href="#license">License</a></li>
    <li><a href="#contact">Contact</a></li>
  </ol>
</details>


<!-- ABOUT THE PROJECT -->
## About The Project

- This is a Matrix bot for a social credit system.


<!-- SETUP -->
## Setup
- Use the example docker-compose.yml file to setup the bot.
- The bot user can be created with Element / Element Web or any other Matrix client that supports registering a new user.

### Environment Variables
- INITIAL_SOCIAL_CREDIT: The initial social credit that a user has 
- ADMIN_USERNAME: Username of the user that will be the admin of the social credit system, user needs to be on the MATRIX_HOMESERVER_URL
- MATRIX_USERNAME: Username of the bot user
- MATRIX_PASSWORD: Password of the bot user
- MATRIX_HOMESERVER_URL: Homeserver url of the bot user for example https://matrix.org
- REACTION_LIMIT: Limits the social credit change reactions that are possible within REACTION_TIMESPAN
- REACTION_TIMESPAN: Timespan in minutes for the REACTION_LIMIT, like a cooldown
- DB_PATH: Path to the database file

### Commands
- !help: Shows the help message
- !list: Lists all users and their social credit for the current room
- !list-emoji: Lists all emojis that can be used to change the social credit for the current room
- !register-emoji: To register an emoji

### Usage
- React with a registered emoji to a message to change the social credit of the user that sent the message

<!-- LICENSE -->
## License

Distributed under the GNU General Public License v3 See `LICENSE` for more information.



<!-- CONTACT -->
## Contact

Julian Hackinger - dev@hackinger.net

Project Link: [https://github.com/0verEngineer/matrix-social-credit](https://github.com/0verEngineer/matrix-social-credits)



<!-- MARKDOWN LINKS & IMAGES -->
[contributors-shield]: https://img.shields.io/github/contributors/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[contributors-url]: https://github.com/0verEngineer/matrix-social-credits/graphs/contributors
[forks-shield]: https://img.shields.io/github/forks/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[forks-url]: https://github.com/0verEngineer/matrix-social-credits/network/members
[stars-shield]: https://img.shields.io/github/stars/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[stars-url]: https://github.com/0verEngineer/matrix-social-credits/stargazers
[issues-shield]: https://img.shields.io/github/issues/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[issues-url]: https://github.com/0verEngineer/matrix-social-credits/issues
[license-shield]: https://img.shields.io/github/license/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[license-url]: https://github.com/0verEngineer/matrix-social-credits/blob/master/LICENSE.txt
